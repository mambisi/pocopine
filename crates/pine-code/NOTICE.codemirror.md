# CodeMirror reference attribution

`pine-code` is a Rust implementation of the bounded native editor described in
RFC-124. It does not embed CodeMirror's JavaScript or claim API compatibility.
The following MIT-licensed sources inform its transaction boundaries, DOM
reader, composition handling, line tokenizer contract, and regression cases:

| Package | Reference commit | Source paths |
| --- | --- | --- |
| state | `ed280c375b165a9ab895bd942eb0529fbc8f7549` | `src/change.ts`, `src/selection.ts`; `test/test-change.ts`, `test/test-selection.ts` |
| view | `7b45dd4462cbf552e5b54df845dd2051662cd242` | `src/input.ts`, `src/domreader.ts`, `src/domobserver.ts`; `test/webtest-composition.ts`, `test/webtest-domchange.ts` |
| language | `89974ce5d39539ce6c5cfea5278443fa9381cbf2` | `src/stream-parser.ts`, `src/stringstream.ts` |
| commands | `ca6f705256b3dc0745b843580ef66091d0c625c1` | `src/history.ts`, `src/commands.ts`; `test/test-history.ts` |

Upstream: <https://codemirror.net/>. Pinned repositories are listed in RFC-124.
The UTF-8 model, bounded storage, transaction owner, and Pocopine lifecycle
integration are implemented here independently. Local reference checkouts
under `tmp/CodeMirror/` are not distributed with this crate.

## Upstream license

MIT License

Copyright (C) 2018-2021 by Marijn Haverbeke <marijn@haverbeke.berlin> and others

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
