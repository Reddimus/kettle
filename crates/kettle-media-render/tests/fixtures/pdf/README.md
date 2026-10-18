# PDF fixtures

`nested_object_streams.pdf` is upstream hayro's regression file for
LaurenzV/hayro#1509: a chain of object streams, each stored inside the next,
which hayro-syntax 0.8.0 resolves by recursing once per level until the stack
overflows. It is copied unchanged from `hayro-tests/pdfs/load/` at commit
a1569b33046900e08aa42b801e5f3655030069fb (LaurenzV/hayro#1510, Apache-2.0 OR
MIT), the fix the vendored hayro-syntax backports.

| File | Bytes | SHA-256 |
| --- | --- | --- |
| nested_object_streams.pdf | 939 | `bf4413a47b6056d60b778f8d51d869919ab736117e163157db7024710c43bee0` |
