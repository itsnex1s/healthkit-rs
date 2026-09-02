# The Apple Health export format

Notes gathered while writing `healthkit-rs`, checked against exports from 2019,
2024 and 2026. Apple documents none of this, so everything below comes from
reading real files.

## There is no schema

Apple publishes neither a DTD nor documentation for the export format. HealthKit
type identifiers are documented; the container is not, and it has changed
between iOS versions without announcement.

The format version is written as a comment inside the `DOCTYPE`:

```xml
<!-- HealthKit Export Version: 13 -->
```

## The archive

```
apple_health_export/
├── export.xml              the bulk of it
├── export_cda.xml          clinical records, FHIR
├── workout-routes/*.gpx    GPS traces
└── electrocardiograms/*.csv
```

**The filename depends on the device locale** — a Chinese export contains
`导出.xml`. Locate it by content instead: `<HealthData ` or
`<!DOCTYPE HealthData` within the first kilobyte.

## Inline DTD

`export.xml` carries an inline DTD in its `DOCTYPE`. For years this broke strict
parsers with `ATTLIST: no name for Attribute` and `DOCTYPE improperly
terminated`; some of it was fixed in iOS 16.2.

Event-based parsers that emit the DTD without interpreting it — `quick-xml`
among them — are unaffected. A test fixture carries a full DTD to keep it
that way.

Comments also appear inside the DOCTYPE and in the document body.

## Timestamps are not ISO 8601

```
2026-05-01 09:14:23 -0700
```

A space instead of `T`, and a space before the offset. Off-the-shelf ISO parsers
fail; the format has to be given explicitly as `%Y-%m-%d %H:%M:%S %z`.

**The timestamp is written in the zone where the sample was recorded.** The first
10 characters of `startDate` are therefore already the local date. Converting to
UTC is a bug: it shifts days for anyone who travels.

## Elements

| Element | Notes |
|---|---|
| `Record` | the overwhelming majority; one data point each |
| `Workout` | totals plus nested `WorkoutEvent`, `WorkoutStatistics`, `WorkoutRoute` |
| `ActivitySummary` | one per day, the activity rings |
| `Correlation` | **contains nested `Record` children** |
| `ClinicalRecord` | references FHIR resources in `export_cda.xml` |

`Record` attributes: `type`, `sourceName`, `sourceVersion`, `device`, `unit`,
`creationDate`, `startDate`, `endDate`, `value`, plus nested `MetadataEntry`.

`device` contains escaped angle brackets:

```
&lt;&lt;HKDevice: 0x282a45810&gt;, name:Apple Watch, manufacturer:Apple, ...&gt;
```

### Correlation is the trap

Blood pressure exists **only** inside `Correlation`:

```xml
<Correlation type="HKCorrelationTypeIdentifierBloodPressure" ...>
  <Record type="HKQuantityTypeIdentifierBloodPressureSystolic" value="118" .../>
  <Record type="HKQuantityTypeIdentifierBloodPressureDiastolic" value="76" .../>
</Correlation>
```

A parser that only looks at top-level elements loses it entirely.

## Why the file is so large

Every record repeats its full attribute set — source, version, device string,
timezone, unit — for a single number. One heart-rate reading costs roughly 200
bytes of markup. Typical sizes:

| Usage | Unzipped |
|---|---|
| 1 year, light | 30–80 MB |
| 5 years + Apple Watch | 200–500 MB |
| 10+ years, several apps | 800 MB – 1.5 GB |

## Sleep is written as overlapping intervals

A single night is recorded as many `HKCategoryTypeIdentifierSleepAnalysis`
records that overlap, and often repeat byte-for-byte:

```xml
<Record ... startDate="2024-01-16 00:09:00 +0800" endDate="2024-01-16 03:42:00 +0800"
        value="HKCategoryValueSleepAnalysisInBed"/>
<Record ... startDate="2024-01-16 00:09:00 +0800" endDate="2024-01-16 03:42:00 +0800"
        value="HKCategoryValueSleepAnalysisInBed"/>
```

One real export had **98 records for one night**. Adding their durations gives
21,079 minutes — 351 hours in a 24-hour day. Intervals must be sorted and merged.

## Sources overlap too

The iPhone and the Apple Watch both count steps, independently, for the same
day. Apple applies a source priority when displaying data; that priority is
**not present in the export**. Adding every source doubles the day.

## What the community schema is missing

The only public XSD
([redeyejedi31/Apple-Health-data](https://github.com/redeyejedi31/Apple-Health-data),
updated 2026-06) covers `HealthData`, `ExportDate`, `Me`, `Record`,
`MetadataEntry`, `Workout`, `WorkoutEvent` and `ActivitySummary` — and disagrees
with reality in places:

| Actually occurs | In the schema |
|---|---|
| `WorkoutStatistics` inside `Workout` (`sum` / `average` / `minimum` / `maximum`) | no |
| `Correlation` with nested `Record` | no |
| `WorkoutRoute` with `Location` or a `FileReference` to GPX | no |
| `HeartRateVariabilityMetadataList` | no |
| `appleMoveTime`, `appleMoveTimeGoal` in `ActivitySummary` | no |
| `Me` uses `HKCharacteristicTypeIdentifier*` | says `dateOfBirth`, `biologicalSex` — wrong |
| comments inside `DOCTYPE` and the body | — |
