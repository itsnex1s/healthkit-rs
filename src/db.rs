use anyhow::Result;
use rusqlite::{params, Connection};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS daily_metrics (
    date    TEXT NOT NULL,          -- YYYY-MM-DD, local date of the sample
    metric  TEXT NOT NULL,          -- StepCount, HeartRateVariabilitySDNN, ...
    unit    TEXT,
    sum     REAL NOT NULL,
    avg     REAL NOT NULL,
    min     REAL NOT NULL,
    max     REAL NOT NULL,
    samples INTEGER NOT NULL,
    sources INTEGER NOT NULL,       -- how many sources wrote this metric that day
    PRIMARY KEY (date, metric)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS sleep_stages (
    date     TEXT NOT NULL,         -- wake-up date, not the date of falling asleep
    stage    TEXT NOT NULL,         -- Core, Deep, REM, Awake, InBed, Unspecified
    minutes  REAL NOT NULL,
    episodes INTEGER NOT NULL,
    PRIMARY KEY (date, stage)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS workouts_daily (
    date        TEXT NOT NULL,
    activity    TEXT NOT NULL,      -- Running, Cycling, ... without the prefix
    sessions    INTEGER NOT NULL,
    minutes     REAL NOT NULL,
    distance_km REAL NOT NULL,
    energy_kcal REAL NOT NULL,
    PRIMARY KEY (date, activity)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS activity_summary (
    date              TEXT PRIMARY KEY,
    active_energy     REAL,
    active_energy_goal REAL,
    exercise_min      REAL,
    exercise_goal     REAL,
    stand_hours       REAL,
    stand_goal        REAL,
    move_min          REAL,         -- appleMoveTime, added in later iOS versions
    move_goal         REAL
) WITHOUT ROWID;
"#;

pub fn open(path: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // WAL with fsync off: the import is idempotent, so an interrupted run is
    // simply repeated rather than recovered.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

#[derive(Default, Clone, Copy)]
pub struct Acc {
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub n: u64,
}

impl Acc {
    pub fn push(&mut self, v: f64) {
        if self.n == 0 {
            self.min = v;
            self.max = v;
        } else {
            if v < self.min {
                self.min = v;
            }
            if v > self.max {
                self.max = v;
            }
        }
        self.sum += v;
        self.n += 1;
    }

    pub fn merge(&mut self, o: &Acc) {
        if o.n == 0 {
            return;
        }
        if self.n == 0 {
            *self = *o;
            return;
        }
        self.sum += o.sum;
        self.n += o.n;
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }

    pub fn avg(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.sum / self.n as f64
        }
    }
}

pub struct MetricRow {
    pub date: String,
    pub metric: String,
    pub unit: Option<String>,
    pub acc: Acc,
    pub sources: usize,
}

pub struct WorkoutRow {
    pub date: String,
    pub activity: String,
    pub sessions: u64,
    pub minutes: f64,
    pub distance_km: f64,
    pub energy_kcal: f64,
}

#[derive(Default)]
pub struct SummaryRow {
    pub date: String,
    pub active_energy: Option<f64>,
    pub active_energy_goal: Option<f64>,
    pub exercise_min: Option<f64>,
    pub exercise_goal: Option<f64>,
    pub stand_hours: Option<f64>,
    pub stand_goal: Option<f64>,
    pub move_min: Option<f64>,
    pub move_goal: Option<f64>,
}

/// UPSERT everywhere: re-importing the same archive changes nothing, and a
/// newer archive overwrites overlapping days.
pub fn write_metrics(conn: &mut Connection, rows: Vec<MetricRow>) -> Result<usize> {
    let tx = conn.transaction()?;
    let n = rows.len();
    {
        let mut stmt = tx.prepare(
            "INSERT INTO daily_metrics (date, metric, unit, sum, avg, min, max, samples, sources)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(date, metric) DO UPDATE SET
                unit=excluded.unit, sum=excluded.sum, avg=excluded.avg, min=excluded.min,
                max=excluded.max, samples=excluded.samples, sources=excluded.sources",
        )?;
        for r in rows {
            stmt.execute(params![
                r.date,
                r.metric,
                r.unit,
                r.acc.sum,
                r.acc.avg(),
                r.acc.min,
                r.acc.max,
                r.acc.n as i64,
                r.sources as i64
            ])?;
        }
    }
    tx.commit()?;
    Ok(n)
}

pub fn write_sleep(conn: &mut Connection, rows: Vec<(String, String, f64, u64)>) -> Result<usize> {
    let tx = conn.transaction()?;
    let n = rows.len();
    {
        let mut stmt = tx.prepare(
            "INSERT INTO sleep_stages (date, stage, minutes, episodes) VALUES (?1,?2,?3,?4)
             ON CONFLICT(date, stage) DO UPDATE SET
                minutes=excluded.minutes, episodes=excluded.episodes",
        )?;
        for (d, s, m, e) in rows {
            stmt.execute(params![d, s, m, e as i64])?;
        }
    }
    tx.commit()?;
    Ok(n)
}

pub fn write_workouts(conn: &mut Connection, rows: Vec<WorkoutRow>) -> Result<usize> {
    let tx = conn.transaction()?;
    let n = rows.len();
    {
        let mut stmt = tx.prepare(
            "INSERT INTO workouts_daily (date, activity, sessions, minutes, distance_km, energy_kcal)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(date, activity) DO UPDATE SET
                sessions=excluded.sessions, minutes=excluded.minutes,
                distance_km=excluded.distance_km, energy_kcal=excluded.energy_kcal",
        )?;
        for r in rows {
            stmt.execute(params![
                r.date,
                r.activity,
                r.sessions as i64,
                r.minutes,
                r.distance_km,
                r.energy_kcal
            ])?;
        }
    }
    tx.commit()?;
    Ok(n)
}

pub fn write_summaries(conn: &mut Connection, rows: Vec<SummaryRow>) -> Result<usize> {
    let tx = conn.transaction()?;
    let n = rows.len();
    {
        let mut stmt = tx.prepare(
            "INSERT INTO activity_summary
                (date, active_energy, active_energy_goal, exercise_min, exercise_goal,
                 stand_hours, stand_goal, move_min, move_goal)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(date) DO UPDATE SET
                active_energy=excluded.active_energy,
                active_energy_goal=excluded.active_energy_goal,
                exercise_min=excluded.exercise_min, exercise_goal=excluded.exercise_goal,
                stand_hours=excluded.stand_hours, stand_goal=excluded.stand_goal,
                move_min=excluded.move_min, move_goal=excluded.move_goal",
        )?;
        for r in rows {
            stmt.execute(params![
                r.date,
                r.active_energy,
                r.active_energy_goal,
                r.exercise_min,
                r.exercise_goal,
                r.stand_hours,
                r.stand_goal,
                r.move_min,
                r.move_goal
            ])?;
        }
    }
    tx.commit()?;
    Ok(n)
}
