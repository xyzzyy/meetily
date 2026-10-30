"""Build the small ONNX pooling regression fixture (requires the onnx package).

No model weights or speech data are used. Run this script from any directory.
"""
from pathlib import Path
import onnx
from onnx import TensorProto as T, helper as h

nodes = [
    h.make_node('Pad', ['input', 'pads'], ['padded'], mode='constant'),
    h.make_node('AveragePool', ['padded'], ['output'],
                kernel_shape=[100], strides=[100], ceil_mode=1),
]
graph = h.make_graph(
    nodes, 'ceil-mode-pool',
    [h.make_tensor_value_info('input', T.FLOAT, [1, 1, 349])],
    [h.make_tensor_value_info('output', T.FLOAT, [1, 1, 4])],
    [h.make_tensor('pads', T.INT64, [6], [0] * 6)],
)
model = h.make_model(graph, opset_imports=[h.make_opsetid('', 14)],
                     producer_name='meetily regression fixture')
model.ir_version = 8
onnx.checker.check_model(model)
onnx.save(model, Path(__file__).with_suffix('.onnx'))
