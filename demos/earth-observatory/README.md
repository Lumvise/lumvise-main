# Earth Observatory

A non-code Lumvise demo for a research team assembling an Earth science briefing.
Documents, measurements, geographic records, and imagery share one project;
knowledge is attached to the relevant sources inside Lumvise.

## Source collection

| File | Format | Source | Role |
| --- | --- | --- | --- |
| `documents/earth-observation-demo.pdf` | PDF | Original demo document, CC0 | Two-page reference for PDF text highlighting |
| `measurements/mauna-loa-monthly-co2.csv` | CSV | NOAA GML / Scripps | Monthly atmospheric measurements |
| `measurements/mauna-loa-annual-co2.txt` | Plain text | NOAA GML / Scripps | Annual measurements with source notes |
| `geography/earthquakes-m2.5-week.geojson` | GeoJSON | USGS earthquake feed | Geographic event snapshot |
| `imagery/observation-islands.png` | PNG | Original AI-generated illustration, CC0 dedication | Fictional landscape for image-region selection |
| `media/flower.mp4` | MP4 | MDN video example | Video playback and time-segment selection |
| `media/t-rex-roar.mp3` | MP3 | MDN audio example | Audio playback and time-segment selection |
| `sources.json` | JSON | Download manifest | Source URLs, attribution, retrieval times, and SHA-256 checksums |

Downloaded files are preserved as received. The PDF is original CC0 demo material.
The earthquake feed is a frozen snapshot, not a live alert service.
These sources cover different places and periods; their presence in
one project does not imply a causal relationship.

## Explore in Lumvise

Select the **earth-observatory** project. Browse its source folders and open the
knowledge attached to each file. Start with **Earth Observatory: guided tour**.

Try asking:

- What kinds of evidence are collected in this project?
- What do the NOAA CSV columns mean, and how are missing values represented?
- Which earthquake snapshot was downloaded, and how fresh is it?
- Why is the generated illustration not evidence of a real place?
- What remains to be checked before using this collection in a briefing?

Example definitions, annotations, a report, a decision, a task, and a project
specification live in the app's Knowledge store, not in sidecar Markdown files.
Their dependency links connect explanations to source files and other knowledge.

The PDF has selectable extracted passages with exact source-text highlights.
CSV content is converted to Markdown for semantic indexing. Image regions and
audio/video segments have curated example annotations in Knowledge. The MDN
clips demonstrate playback only; they are not Earth science evidence.

Scanned pages still require OCR. Region and time annotations are curated, not
automatic image understanding or transcription. No geographic analysis is implied.

## Provenance and reuse

See **[REUSE.md](REUSE.md)** for per-file reuse terms and required credits, and
`sources.json` for license evidence, immutable media references, and checksums.
The PDF, generated illustration and both media clips use **CC0 1.0 Universal**.
The image depicts fictional geography, not a satellite observation. The MP3 is
a sound effect, not a spoken-voice recording. NOAA and USGS datasets
retain their documented agency reuse terms and credits. No agency
endorsement is implied. Keep these notices with redistributed copies.

The former USGS illustrated handbook was removed because its third-party
illustration rights were not established. Public availability alone is not an
acceptable basis for adding files to this demo.

The parent repository ignores `demos/`, so this is a local demonstration project.
Copying this folder alone does not transfer its database-backed knowledge.
