Changed
- An unknown key under `llm:` is now a config load error, as under `llm.jev:`. A `jev` option written one level too high, such as `llm.payload_dump_dir`, used to be ignored, so a run meant to write a payload dump sent live requests instead (#111).
