# anchor-editor

Web tool for placing and naming favourite anchoring points on the Stockholm archipelago map.

## Run

```
cargo run -p anchor-editor
```

Open http://localhost:3030 in a browser.

## Usage

- Click inside the blue bbox rectangle to add a point — you'll be prompted for a name and optional note.
- Grey anchors are pre-loaded from the existing `anchorings.csv`; red anchors are new ones you've added this session.
- Use **Copy as CSV** or **Download CSV** to export, then drop the file at the repo root as `anchorings.csv`.

## Config

| Env var | Default | Description |
|---|---|---|
| `ANCHOR_EDITOR_ADDR` | `127.0.0.1:3030` | Listen address |
| `BLACK_SEA_ANCHORINGS` | `anchorings.csv` | Path to existing CSV to pre-load |
