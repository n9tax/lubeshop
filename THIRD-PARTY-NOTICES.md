# Third-party notices

The Lube Shop is licensed under GPL-3.0-or-later (see [`COPYING`](COPYING)). It
also **includes** and **redistributes** third-party software, whose notices and
licenses are collected here.

Rust crate dependencies (linked into the binary) are listed separately in
[`THIRD-PARTY-LICENSES`](THIRD-PARTY-LICENSES.html), generated from the
dependency tree.

---

## Ported source code

### trs80-base (Model I/III/4 DMK + TRSDOS decoders)

`crates/gwm-core/src/trs_disk.rs` is a Rust port of the `DmkFloppyDisk` and
`Trsdos` decoders from the **trs80-base** TypeScript library by Lawrence
Kesteloot (<https://github.com/lkesteloot/trs80>), used under the MIT License:

```
MIT License

Copyright (c) 2021 Lawrence Kesteloot

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## Redistributed tool binaries (Windows bundles)

On Linux the external tools are installed from the user's own package manager or
built from source **on the user's machine** — we don't redistribute them. On
**Windows**, some tools have no package, so the app downloads prebuilt bundles we
host. Those bundles include, and are governed by, the upstream licenses below; the
corresponding source is available at the linked, version-pinned upstreams. Each
bundle zip also ships a copy of the tool's own `LICENSE`.

| Tool | License | Source |
|------|---------|--------|
| cpmtools | GPL-2.0-or-later | <http://www.moria.de/~michael/cpmtools/> |
| mtools | GPL-3.0-or-later | <https://www.gnu.org/software/mtools/> |
| AppleCommander | GPL-2.0 | <https://github.com/AppleCommander/AppleCommander> |
| amitools | GPL-2.0 | <https://github.com/cnvogelg/amitools> |
| Eclipse Temurin JRE (bundled with AppleCommander) | GPL-2.0 with Classpath Exception | <https://adoptium.net/> |

**HxC (`hxcfe`)** is *not* redistributed by us — the app installs it directly from
the author's official download (HxC2001, © Jean-François Del Nero), on every
platform, because its source has no license granting redistribution.
