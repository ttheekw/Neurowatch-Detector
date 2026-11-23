# Models

`yolox_nano.onnx` and its class file `yolox_nano.json` are built into `nw-detect` (`include_bytes!` / `include_str!` in `src/detector.rs` and `src/classes.rs`).

| File | Source | SHA-256 | Licence |
|---|---|---|---|
| `yolox_nano.onnx` | https://github.com/Megvii-BaseDetection/YOLOX/releases/download/0.1.1rc0/yolox_nano.onnx | `c789161ed43c8269fcd4e67c67eeeb4e80c622da2eb296a20bc6007bd18a0b7d` | Apache-2.0 (Megvii Inc.) |

YOLOX-Nano takes a 416×416 picture and finds the 80 COCO classes. The model
defines what the plugin can detect; each camera chooses which classes to report
and a confidence for each.

## Class file

Every model has a class file naming its output classes in the model's output
order (class `i` is output column `5 + i`):

```json
{"name": "YOLOX-Nano", "classes": [
  {"id": "person", "label": "Person", "default": true},
  {"id": "bicycle", "label": "Bicycle"},
  {"id": "car", "label": "Car", "default": true, "confidence": 50}
]}
```

- `id`: 1–32 characters of `a-z`, `0-9` and `_`, unique. It is the event kind.
- `label`: 1–64 characters, what people read.
- `default` (optional, default false): chosen on a new camera.
- `confidence` (optional, 10–95, default 50): the class's starting confidence.

`--model PATH` reads the class file at `PATH` with the extension replaced by
`.json` (`yolox_tiny.onnx` uses `yolox_tiny.json`); `--classes FILE` names
another. A missing or invalid class file, or a class count that differs from
the model's output columns minus 5, stops the daemon at start with an error.

Other YOLOX exports from the same release (all trained on the same 80 COCO
classes, so they can use a copy of `yolox_nano.json`) work with `--model PATH`,
for more accuracy at more processing:

| File | SHA-256 | Measured on the development PC (CPU, one 640×360 picture) |
|---|---|---|
| `yolox_tiny.onnx` (416×416) | `427cc366d34e27ff7a03e2899b5e3671425c262ea2291f88bb942bc1cc70b0f7` | about 130 ms, against about 40 ms for Nano |
| `yolox_s.onnx` (640×640) | (not checked here) | |

Do not use Ultralytics YOLOv5/v8/v11 weights here: they are AGPL-3.0, which
NeuroWatch's Apache-2.0 distribution cannot include.
