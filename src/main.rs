mod db;
mod xml;

use anyhow::{bail, Result};
use clap::Parser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "healthkit-rs",
    about = "Apple Health export.zip to daily aggregates in SQLite",
    version
)]
struct Args {
    /// export.zip from the Health app (or export.xml itself, with --xml)
    input: PathBuf,
    /// SQLite database file (created if missing)
    db: PathBuf,
    /// input is an already-extracted export.xml
    #[arg(long)]
    xml: bool,
}

/// The filename inside the archive depends on the device locale (导出.xml in
/// Chinese), so we search by content rather than by name.
fn find_export_xml<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>) -> Result<String> {
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| n.ends_with(".xml") && n.matches('/').count() <= 1)
        .collect();

    for name in names {
        let mut head = [0u8; 1024];
        let n = zip.by_name(&name)?.read(&mut head)?;
        let head = &head[..n];
        if find(head, b"<HealthData ").is_some() || find(head, b"<!DOCTYPE HealthData").is_some() {
            return Ok(name);
        }
    }
    bail!("no export.xml with a <HealthData> root found in the archive")
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut conn = db::open(&args.db)?;

    let stats = if args.xml {
        let f = File::open(&args.input)?;
        xml::ingest(BufReader::new(f), &mut conn)?
    } else {
        let f = File::open(&args.input)?;
        let mut zip = zip::ZipArchive::new(f)?;
        let name = find_export_xml(&mut zip)?;
        eprintln!("reading {name}");
        let entry = zip.by_name(&name)?;
        xml::ingest(BufReader::new(entry), &mut conn)?
    };

    println!(
        "read: {} records, {} workouts, {} summaries ({} skipped)",
        stats.records, stats.workouts, stats.summaries, stats.skipped
    );
    println!(
        "wrote: daily_metrics {}, sleep_stages {}, workouts_daily {}, activity_summary {}",
        stats.metric_rows, stats.sleep_rows, stats.workout_rows, stats.summary_rows
    );
    Ok(())
}
