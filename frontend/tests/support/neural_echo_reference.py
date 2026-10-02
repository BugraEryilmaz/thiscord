"""Independent offline validation only; Python/LiteRT are not app dependencies.
Usage: python neural_echo_reference.py model.tflite reference.json
Requires ai-edge-litert==2.2.0 and numpy. No downloads or microphone access.
"""
import hashlib
import json
import sys
from pathlib import Path
import numpy as np
from ai_edge_litert.interpreter import Interpreter, OpResolverType

path, output = map(Path, sys.argv[1:])
digest = hashlib.sha256(path.read_bytes()).hexdigest()
assert digest == "3a18833eaeb08bfffb88a588c10db67885f246ba4794fd0f1609f2ccf6c30b77"
engine = Interpreter(model_path=str(path), num_threads=1,
    experimental_op_resolver_type=OpResolverType.BUILTIN_WITHOUT_DEFAULT_DELEGATES)
engine.allocate_tensors()
run = engine.get_signature_runner("serving_default")
state = np.zeros((3, 3, 2, 48), dtype=np.float32)
frames = []
for i in range(100):
    a = np.array([((i*17+j*31) % 997)/997 for j in range(129)], dtype=np.float32)
    b = np.array([((i*23+j*7) % 991)/991 for j in range(129)], dtype=np.float32)
    if i < 5 or i >= 90:
        a.fill(0); b.fill(0)
    result = run(cancelled_frame=a, ref_frame=b, mic_frame=np.zeros(129, dtype=np.float32), lstm_state=state)
    state = result["lstm_state"] * np.float32(0.999)
    bits=lambda x: x.flatten().view(np.uint32).tolist()
    frames.append(dict(cancelled=bits(a), reference=bits(b),
        mask=bits(result["echo_mask_frame"]), unbounded=bits(result["unbounded_echo_mask_frame"]), state=bits(state)))
output.write_text(json.dumps(dict(sha256=digest, frames=frames)))
print("Wrote 100 recurrent reference frames; no model weights included.")
