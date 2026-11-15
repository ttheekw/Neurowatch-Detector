# Models

`yolox_nano.onnx` is built into `nw-detect` (`include_bytes!` in `src/detector.rs`).

| File | Source | SHA-256 | Licence |
|---|---|---|---|
| `yolox_nano.onnx` | https://github.com/Megvii-BaseDetection/YOLOX/releases/download/0.1.1rc0/yolox_nano.onnx | `c789161ed43c8269fcd4e67c67eeeb4e80c622da2eb296a20bc6007bd18a0b7d` | Apache-2.0 (Megvii Inc.) |

YOLOX-Nano takes a 416×416 picture and finds the 80 COCO classes; the plugin
reports people, vehicles (bicycle, car, motorcycle, bus, truck) and animals
(bird, cat, dog, horse, sheep, cow, elephant, bear, zebra, giraffe).

Other YOLOX exports from the same release work with `--model PATH`, for more
accuracy at more processing:

| File | SHA-256 | Measured on the development PC (CPU, one 640×360 picture) |
|---|---|---|
| `yolox_tiny.onnx` (416×416) | `427cc366d34e27ff7a03e2899b5e3671425c262ea2291f88bb942bc1cc70b0f7` | about 130 ms, against about 40 ms for Nano |
| `yolox_s.onnx` (640×640) | (not checked here) | |

Do not use Ultralytics YOLOv5/v8/v11 weights here: they are AGPL-3.0, which
NeuroWatch's Apache-2.0 distribution cannot include.
