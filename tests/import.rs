use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_healthkit-rs")
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hkrs-{}-{}.db", std::process::id(), name));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", p.display(), suffix));
    }
    p
}

/// Run the real binary: assert on what reaches the database rather than on
/// the behaviour of functions in isolation.
fn run(fixture: &str, db: &Path, as_xml: bool) -> String {
    let mut args = vec![fixture.to_string(), db.display().to_string()];
    if as_xml {
        args.push("--xml".into());
    }
    let out = Command::new(bin())
        .args(&args)
        .output()
        .expect("binary failed to start");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

// ─────────────────────────────────────────────────────────────────────
// Older format (Export Version 9): escaped `device`, a third-party source
// with metadata, two workouts in one day, miles and metres.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn legacy_export() {
    let db = tmp("legacy");
    run("tests/fixtures/legacy-export.xml", &db, true);
    let c = Connection::open(&db).unwrap();

    let bmi: f64 = one(
        &c,
        "SELECT sum FROM daily_metrics WHERE metric='BodyMassIndex'",
    );
    assert_eq!(bmi, 22.5);
    let hr: f64 = one(&c, "SELECT sum FROM daily_metrics WHERE metric='HeartRate'");
    assert_eq!(hr, 72.0);

    // Two runs in one day add up, units normalised to kilometres:
    // 2.5 mi = 4.02336 km plus 1000 m = 1 km.
    let (sessions, km, minutes, kcal): (i64, f64, f64, f64) = c
        .query_row(
            "SELECT sessions, distance_km, minutes, energy_kcal FROM workouts_daily
             WHERE activity='Running'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(sessions, 2);
    assert!((km - 5.023_36).abs() < 1e-5, "units not normalised: {km}");
    assert_eq!(minutes, 34.5);
    assert_eq!(kcal, 400.5);

    // WorkoutRoute — both with a FileReference and with nested Location —
    // must not break parsing, even though we do not import the points.
    let routes: i64 = one(&c, "SELECT count(*) FROM workouts_daily");
    assert_eq!(routes, 1);

    let n: i64 = one(&c, "SELECT count(*) FROM activity_summary");
    assert_eq!(n, 2);
    let stand: f64 = one(
        &c,
        "SELECT stand_hours FROM activity_summary WHERE date='2019-06-11'",
    );
    assert_eq!(stand, 13.0);
}

// ─────────────────────────────────────────────────────────────────────
// Elements absent from both the 2019 export and the only public community
// XSD: WorkoutStatistics, Correlation with nested Record, appleMoveTime,
// comments inside the DOCTYPE and the body.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn modern_elements() {
    let db = tmp("modern");
    run("tests/fixtures/modern-elements.xml", &db, true);
    let c = Connection::open(&db).unwrap();

    // Blood pressure lives ONLY inside Correlation. A parser that reads only
    // the top level loses it entirely.
    let sys: f64 = one(
        &c,
        "SELECT sum FROM daily_metrics WHERE metric='BloodPressureSystolic'",
    );
    let dia: f64 = one(
        &c,
        "SELECT sum FROM daily_metrics WHERE metric='BloodPressureDiastolic'",
    );
    assert_eq!(sys, 118.0, "Record inside Correlation was lost");
    assert_eq!(dia, 76.0);

    // appleMoveTime is a later-iOS field, missing from the community XSD
    let move_min: f64 = one(
        &c,
        "SELECT move_min FROM activity_summary WHERE date='2026-08-20'",
    );
    assert_eq!(move_min, 41.0);

    // WorkoutStatistics inside Workout must not break parsing
    let (min, km, kcal): (f64, f64, f64) = c
        .query_row(
            "SELECT minutes, distance_km, energy_kcal FROM workouts_daily WHERE activity='Cycling'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((min, km, kcal), (45.0, 12.5, 410.0));
}

// ─────────────────────────────────────────────────────────────────────
// A night crossing midnight belongs entirely to the wake-up date.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn sleep_belongs_to_wake_date() {
    let db = tmp("sleep");
    run("tests/fixtures/modern-elements.xml", &db, true);
    let c = Connection::open(&db).unwrap();

    let dates: i64 = one(&c, "SELECT count(DISTINCT date) FROM sleep_stages");
    assert_eq!(dates, 1, "one night was split across two dates");
    let d: String = one(&c, "SELECT DISTINCT date FROM sleep_stages");
    assert_eq!(d, "2026-08-21", "sleep filed under the falling-asleep date");

    // 23:10→02:10 Core, 02:10→03:25 Deep, 03:25→06:25 REM
    let core: f64 = one(&c, "SELECT minutes FROM sleep_stages WHERE stage='Core'");
    assert_eq!(core, 180.0);
    let deep: f64 = one(&c, "SELECT minutes FROM sleep_stages WHERE stage='Deep'");
    assert_eq!(deep, 75.0);
    let rem: f64 = one(&c, "SELECT minutes FROM sleep_stages WHERE stage='REM'");
    assert_eq!(rem, 180.0);
}

// ─────────────────────────────────────────────────────────────────────
// iPhone and Watch count steps independently. Adding them doubles the day.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn cumulative_metrics_dedupe_across_sources() {
    let db = tmp("sources");
    run("tests/fixtures/two-sources.xml", &db, true);
    let c = Connection::open(&db).unwrap();

    let (sum, sources): (f64, i64) = c
        .query_row(
            "SELECT sum, sources FROM daily_metrics WHERE metric='StepCount'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(sources, 2);
    // Watch: 3500+6000=9500, iPhone: 3000+5000=8000 → keep the larger source
    assert_eq!(
        sum, 9500.0,
        "sources were added instead of one being chosen"
    );

    // Heart rate is instantaneous — sources merge instead of competing
    let (hr_n, hr_sum): (i64, f64) = c
        .query_row(
            "SELECT samples, sum FROM daily_metrics WHERE metric='HeartRate'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(hr_n, 2);
    assert_eq!(hr_sum, 160.0);
}

// ─────────────────────────────────────────────────────────────────────
// Full inline DTD, MetadataEntry, a value-less category, idempotency.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn inline_dtd_and_idempotency() {
    let db = tmp("dtd");
    let first = run("tests/fixtures/basics.xml", &db, true);
    assert!(first.contains("read: 9 records"), "{first}");

    let c = Connection::open(&db).unwrap();
    let steps: f64 = one(
        &c,
        "SELECT sum FROM daily_metrics WHERE date='2026-08-30' AND metric='StepCount'",
    );
    assert_eq!(steps, 2000.0);
    let unit: String = one(
        &c,
        "SELECT unit FROM daily_metrics WHERE metric='HeartRate' LIMIT 1",
    );
    assert_eq!(unit, "count/min");
    drop(c);

    // A second run must not accumulate anything
    run("tests/fixtures/basics.xml", &db, true);
    let c = Connection::open(&db).unwrap();
    let steps2: f64 = one(
        &c,
        "SELECT sum FROM daily_metrics WHERE date='2026-08-30' AND metric='StepCount'",
    );
    assert_eq!(steps2, 2000.0, "re-import doubled the values");
    let rows: i64 = one(&c, "SELECT count(*) FROM daily_metrics");
    assert_eq!(rows, 3);
}

// ─────────────────────────────────────────────────────────────────────
// Archive: the file sits in apple_health_export/ and its name depends on the
// device locale, so it is located by content.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn reads_zip_archive() {
    let db = tmp("zip");
    let out = run("tests/fixtures/export.zip", &db, false);
    assert!(out.contains("read: 2 records"), "{out}");

    let c = Connection::open(&db).unwrap();
    let n: i64 = one(&c, "SELECT count(*) FROM daily_metrics");
    assert_eq!(n, 2);
    let km: f64 = one(&c, "SELECT distance_km FROM workouts_daily");
    assert!((km - 5.023_36).abs() < 1e-5);
}

// ─────────────────────────────────────────────────────────────────────
// Regression from real data: in a live export a single sleep episode is
// written as dozens of overlapping records. Naive summing produced
// 21,079 minutes (351 hours) in one day.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn overlapping_sleep_episodes_are_merged() {
    let db = tmp("overlap");
    run("tests/fixtures/sleep-overlap.xml", &db, true);
    let c = Connection::open(&db).unwrap();

    let (minutes, episodes): (f64, i64) = c
        .query_row(
            "SELECT minutes, episodes FROM sleep_stages WHERE stage='InBed'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    // 00:09→07:09 is 420 minutes, plus a 30-minute nap = 450.
    // Naively adding all seven records would give 1272.
    assert_eq!(minutes, 450.0, "overlaps were not merged");
    assert_eq!(episodes, 2, "the night and the nap must stay separate");
}
