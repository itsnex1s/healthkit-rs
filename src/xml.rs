use crate::db::{self, Acc, MetricRow, SummaryRow, WorkoutRow};
use anyhow::{Context, Result};
use chrono::DateTime;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use rusqlite::Connection;
use std::collections::HashMap;
use std::io::BufRead;

/// Timestamp format in export.xml: `2026-05-01 09:14:23 -0700`.
/// Not ISO 8601: a space instead of `T`, and a space before the offset.
const TS: &str = "%Y-%m-%d %H:%M:%S %z";

const Q_PREFIX: &str = "HKQuantityTypeIdentifier";
const SLEEP: &str = "HKCategoryTypeIdentifierSleepAnalysis";
const ACT_PREFIX: &str = "HKWorkoutActivityType";

/// Metrics Apple accumulates over an interval: their values add up. If both
/// the iPhone and the Watch wrote the same day, summing every source doubles
/// the step count. For these we keep the source with the largest sum; the
/// source priority Apple itself applies is not present in the export.
const CUMULATIVE: &[&str] = &[
    "StepCount",
    "DistanceWalkingRunning",
    "DistanceCycling",
    "DistanceSwimming",
    "DistanceDownhillSnowSports",
    "DistanceWheelchair",
    "ActiveEnergyBurned",
    "BasalEnergyBurned",
    "FlightsClimbed",
    "AppleExerciseTime",
    "AppleStandTime",
    "AppleMoveTime",
    "SwimmingStrokeCount",
    "PushCount",
    "NumberOfTimesFallen",
    "DietaryEnergyConsumed",
    "DietaryWater",
];

pub struct Stats {
    /// The file ended in the middle of an element. Apple's own export does
    /// this: the archive is intact and the XML inside it is cut short. Losing
    /// everything parsed before that point would be the wrong answer.
    pub truncated: bool,
    pub records: u64,
    pub workouts: u64,
    pub summaries: u64,
    pub skipped: u64,
    pub metric_rows: usize,
    pub sleep_rows: usize,
    pub workout_rows: usize,
    pub summary_rows: usize,
}

/// (date, metric, source) → unit and accumulator.
type MetricKey = (String, String, String);
type UnitAcc = (Option<String>, Acc);
type MetricAcc = HashMap<MetricKey, UnitAcc>;

#[derive(Default)]
struct Sink {
    /// The source is part of the key so duplicates between iPhone and Watch
    /// can be collapsed.
    metrics: MetricAcc,
    /// Intervals, not a minute total: in real exports sleep episodes overlap
    /// and repeat, so they cannot be added up directly.
    sleep: HashMap<(String, String), Vec<(i64, i64)>>,
    workouts: HashMap<(String, String), WorkoutRow>,
    summaries: HashMap<String, SummaryRow>,
}

fn attrs(e: &BytesStart) -> HashMap<String, String> {
    e.attributes()
        .flatten()
        .filter_map(|a| {
            let k = String::from_utf8_lossy(a.key.as_ref()).into_owned();
            let v = a.unescape_value().ok()?.into_owned();
            Some((k, v))
        })
        .collect()
}

fn num(m: &HashMap<String, String>, k: &str) -> Option<f64> {
    m.get(k)?.parse().ok()
}

/// Apple writes the timestamp in the zone where the sample was recorded, so
/// the first 10 characters are already the local date. Converting to UTC would
/// shift days for anyone who flies.
fn date_of(ts: &str) -> Option<&str> {
    (ts.len() >= 10).then(|| &ts[..10])
}

fn minutes_between(start: &str, end: &str) -> Option<f64> {
    let (a, b) = epoch_range(start, end)?;
    Some((b - a) as f64 / 60.0)
}

fn epoch_range(start: &str, end: &str) -> Option<(i64, i64)> {
    let a = DateTime::parse_from_str(start, TS).ok()?.timestamp();
    let b = DateTime::parse_from_str(end, TS).ok()?.timestamp();
    (b > a).then_some((a, b))
}

/// Merging overlaps. In a real export a single "in bed" episode is written
/// dozens of times — 98 records for one night, some byte-identical. Summing
/// their durations yields 351 hours in a day.
fn merge_intervals(mut v: Vec<(i64, i64)>) -> (f64, u64) {
    if v.is_empty() {
        return (0.0, 0);
    }
    v.sort_unstable();
    let mut total = 0i64;
    let mut count = 0u64;
    let (mut cs, mut ce) = v[0];
    for &(s, e) in &v[1..] {
        if s <= ce {
            ce = ce.max(e);
        } else {
            total += ce - cs;
            count += 1;
            cs = s;
            ce = e;
        }
    }
    total += ce - cs;
    count += 1;
    (total as f64 / 60.0, count)
}

fn stage_of(value: &str) -> &str {
    match value.rsplit("SleepAnalysis").next().unwrap_or(value) {
        "AsleepCore" => "Core",
        "AsleepDeep" => "Deep",
        "AsleepREM" => "REM",
        "AsleepUnspecified" | "Asleep" => "Unspecified",
        "Awake" => "Awake",
        "InBed" => "InBed",
        other => other,
    }
}

fn to_km(v: f64, unit: Option<&str>) -> f64 {
    match unit {
        Some("mi") => v * 1.609_344,
        Some("m") => v / 1000.0,
        Some("yd") => v * 0.000_914_4,
        _ => v, // km and anything unrecognised
    }
}

fn to_kcal(v: f64, unit: Option<&str>) -> f64 {
    match unit {
        // HealthKit writes both "Cal" and "kcal" — the same thing (kilocalorie).
        Some("J") => v / 4184.0,
        Some("kJ") => v / 4.184,
        _ => v,
    }
}

pub fn ingest<R: BufRead>(input: R, conn: &mut Connection) -> Result<Stats> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut s = Sink::default();
    let (mut records, mut workouts, mut summaries, mut skipped) = (0u64, 0u64, 0u64, 0u64);

    let mut truncated = false;
    loop {
        // Empty and Start are handled the same way: only attributes matter.
        // A Record inside Correlation lands here too, which is correct —
        // otherwise blood pressure, which lives only there, would be lost.
        let ev = match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) => Some(e.to_owned()),
            Ok(Event::Eof) => break,
            Ok(_) => None,
            // A cut-off element at the end of the input. Everything before it
            // is still good, so keep it and say so.
            Err(quick_xml::Error::IllFormed(_)) | Err(quick_xml::Error::Syntax(_)) => {
                truncated = true;
                break;
            }
            Err(e) => return Err(e.into()),
        };
        if let Some(e) = ev {
            match e.name().as_ref() {
                b"Record" => {
                    records += 1;
                    if !take_record(&attrs(&e), &mut s) {
                        skipped += 1;
                    }
                }
                b"Workout" => {
                    workouts += 1;
                    if !take_workout(&attrs(&e), &mut s) {
                        skipped += 1;
                    }
                }
                b"ActivitySummary" => {
                    summaries += 1;
                    if !take_summary(&attrs(&e), &mut s) {
                        skipped += 1;
                    }
                }
                _ => {}
            }
        }
        buf.clear();
    }

    let metric_rows =
        db::write_metrics(conn, collapse_sources(s.metrics)).context("daily_metrics")?;
    let sleep_rows = db::write_sleep(
        conn,
        s.sleep
            .into_iter()
            .map(|((d, st), iv)| {
                let (minutes, episodes) = merge_intervals(iv);
                (d, st, minutes, episodes)
            })
            .collect(),
    )
    .context("sleep_stages")?;
    let workout_rows =
        db::write_workouts(conn, s.workouts.into_values().collect()).context("workouts_daily")?;
    let summary_rows = db::write_summaries(conn, s.summaries.into_values().collect())
        .context("activity_summary")?;

    Ok(Stats {
        truncated,
        records,
        workouts,
        summaries,
        skipped,
        metric_rows,
        sleep_rows,
        workout_rows,
        summary_rows,
    })
}

/// Collapsing sources. Cumulative metrics keep a single source — the one with
/// the largest daily sum; instantaneous ones (heart rate, body mass) merge all.
fn collapse_sources(raw: MetricAcc) -> Vec<MetricRow> {
    let mut by_day: HashMap<(String, String), Vec<UnitAcc>> = HashMap::new();
    for ((date, metric, _src), v) in raw {
        by_day.entry((date, metric)).or_default().push(v);
    }

    by_day
        .into_iter()
        .map(|((date, metric), parts)| {
            let sources = parts.len();
            let cumulative = CUMULATIVE.contains(&metric.as_str());
            let (unit, acc) = if cumulative && sources > 1 {
                parts
                    .into_iter()
                    .max_by(|a, b| a.1.sum.total_cmp(&b.1.sum))
                    .unwrap()
            } else {
                let mut acc = Acc::default();
                let mut unit = None;
                for (u, a) in parts {
                    acc.merge(&a);
                    unit = unit.or(u);
                }
                (unit, acc)
            };
            MetricRow {
                date,
                metric,
                unit,
                acc,
                sources,
            }
        })
        .collect()
}

fn take_record(a: &HashMap<String, String>, s: &mut Sink) -> bool {
    let (Some(rtype), Some(start)) = (a.get("type"), a.get("startDate")) else {
        return false;
    };

    if rtype == SLEEP {
        let (Some(value), Some(end)) = (a.get("value"), a.get("endDate")) else {
            return false;
        };
        let Some(range) = epoch_range(start, end) else {
            return false;
        };
        // The episode belongs to the wake-up date: sleep from 23:30 to 07:00
        // is the night of the new day, not the previous one.
        let Some(date) = date_of(end) else {
            return false;
        };
        s.sleep
            .entry((date.to_string(), stage_of(value).to_string()))
            .or_default()
            .push(range);
        return true;
    }

    let Some(metric) = rtype.strip_prefix(Q_PREFIX) else {
        return false; // categories other than sleep carry no numeric value
    };
    let (Some(date), Some(raw)) = (date_of(start), a.get("value")) else {
        return false;
    };
    let Ok(v) = raw.parse::<f64>() else {
        return false;
    };

    let src = a.get("sourceName").cloned().unwrap_or_default();
    let e = s
        .metrics
        .entry((date.to_string(), metric.to_string(), src))
        .or_insert_with(|| (a.get("unit").cloned(), Acc::default()));
    e.1.push(v);
    true
}

fn take_workout(a: &HashMap<String, String>, s: &mut Sink) -> bool {
    let Some(start) = a.get("startDate") else {
        return false;
    };
    let Some(date) = date_of(start) else {
        return false;
    };
    let activity = a
        .get("workoutActivityType")
        .map(|t| t.strip_prefix(ACT_PREFIX).unwrap_or(t).to_string())
        .unwrap_or_else(|| "Unknown".into());

    // duration comes in minutes or seconds — trust durationUnit, and fall
    // back to the timestamps when it is absent.
    let minutes = match (
        num(a, "duration"),
        a.get("durationUnit").map(String::as_str),
    ) {
        (Some(d), Some("min")) => d,
        (Some(d), Some("s") | Some("sec")) => d / 60.0,
        (Some(d), None) => d,
        _ => a
            .get("endDate")
            .and_then(|e| minutes_between(start, e))
            .unwrap_or(0.0),
    };

    let km = num(a, "totalDistance")
        .map(|d| to_km(d, a.get("totalDistanceUnit").map(String::as_str)))
        .unwrap_or(0.0);
    let kcal = num(a, "totalEnergyBurned")
        .map(|d| to_kcal(d, a.get("totalEnergyBurnedUnit").map(String::as_str)))
        .unwrap_or(0.0);

    let e = s
        .workouts
        .entry((date.to_string(), activity.clone()))
        .or_insert_with(|| WorkoutRow {
            date: date.to_string(),
            activity,
            sessions: 0,
            minutes: 0.0,
            distance_km: 0.0,
            energy_kcal: 0.0,
        });
    e.sessions += 1;
    e.minutes += minutes;
    e.distance_km += km;
    e.energy_kcal += kcal;
    true
}

fn take_summary(a: &HashMap<String, String>, s: &mut Sink) -> bool {
    let Some(date) = a.get("dateComponents") else {
        return false;
    };
    s.summaries.insert(
        date.clone(),
        SummaryRow {
            date: date.clone(),
            active_energy: num(a, "activeEnergyBurned"),
            active_energy_goal: num(a, "activeEnergyBurnedGoal"),
            exercise_min: num(a, "appleExerciseTime"),
            exercise_goal: num(a, "appleExerciseTimeGoal"),
            stand_hours: num(a, "appleStandHours"),
            stand_goal: num(a, "appleStandHoursGoal"),
            move_min: num(a, "appleMoveTime"),
            move_goal: num(a, "appleMoveTimeGoal"),
        },
    );
    true
}
