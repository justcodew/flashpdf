# 渲染性能基准（`render` feature）

flashpdf 在 v0.7 之后引入了**可选的 PDFium 渲染后端**（`render` Cargo feature）。
本文记录 `page.get_pixmap(dpi=150)` 与 PyMuPDF / pypdfium2 的端到端对比，
并做了**阶段拆分**以定位真实瓶颈。

## TL;DR

**165 PDF 渲染第 0 页 150 DPI，154 成功**（11 个 0 页 PDF 三家都失败，已剔除）：

| 库 | 后端 | 总耗时 | p50 ms | 速度倍数 |
|---|---|---:|---:|---:|
| **flashpdf** | PDFium + Rust `image` PNG | **3.56s** | **10.69** | **—** |
| pypdfium2 | PDFium + PIL PNG | 5.81s | 21.02 | 1.63× 慢 |
| fitz (PyMuPDF) | MuPDF + 内置 PNG | 10.34s | 35.03 | 2.90× 慢 |

flashpdf 在**每个文件大小桶**都最快。直觉上"fitz 最快"在渲染场景里**不成立**——
原因见下方拆分。

## 测试环境

- **OS**: macOS (Apple Silicon ARM64)
- **Python**: 3.14
- **flashpdf**: 0.7.0（`maturin develop --release --features render`）
- **fitz (PyMuPDF)**: 1.27.2.3
- **pypdfium2**: 最新 PyPI
- **PDFium binary**: chromium/7906，arm64 macOS
- **样本**: PyMuPDF 测试集的 165-PDF bug-regression 语料
  （`pymupdf/tests/resources`），每个 PDF 是一次历史 bug 的最小复现

## 端到端对比（每个 PDF 渲染第 0 页 150 DPI）

```
library         ok     p50 ms     p90 ms    mean ms      sum s
-------------------------------------------------------------------
flashpdf       154      10.69      43.29      23.15       3.56
fitz           154      35.03      88.55      67.14      10.34
pypdfium2      154      21.02      58.52      37.72       5.81
```

### 按文件大小分桶（p50 ms）

| 桶 | n | flashpdf | fitz | pypdfium2 |
|---|---:|---:|---:|---:|
| <10 KB | 30 | **4.20** | 22.95 | 9.93 |
| 10-100 KB | 47 | **8.08** | 30.48 | 17.04 |
| 100 KB-1 MB | 57 | **15.10** | 43.16 | 28.26 |
| >1 MB | 20 | **32.13** | 53.27 | 36.34 |

### 速度倍数（corpus 总耗时）

- **flashpdf vs fitz**: **2.90×**（3.56s vs 10.34s）
- **flashpdf vs pypdfium2**: **1.63×**（3.56s vs 5.81s）

## 阶段拆分：为什么 flashpdf 最快？

直觉认为 fitz (MuPDF) 应该最快。把渲染流程拆成 **open / raster / PNG encode**
三段单独计时后，真实瓶颈浮出水面。

| 阶段 | flashpdf | fitz | pypdfium2 |
|---|---:|---:|---:|
| open()（fitz/pypdfium2 lazy，flashpdf eager）| 0.46s | 0.07s | 0.04s |
| raster（纯光栅化，不含 PNG）| — | 4.88s | 2.91s |
| PNG 编码 | — | **5.36s** | 2.85s |
| raster + PNG（flashpdf 内部未拆分）| **3.05s** | 10.24s | 5.76s |
| **总计** | **3.52s** | **10.31s** | **5.79s** |

p50 视角更刺眼：

| 阶段（p50 ms） | flashpdf | fitz | pypdfium2 |
|---|---:|---:|---:|
| raster only | — | 5.65 | 6.12 |
| **PNG encode** | — | **27.08** | 13.83 |

### 关键发现

**1. fitz 的 PNG 编码是最大瓶颈（单 PDF p50 27ms）**
- `pix.tobytes("png")` 走 MuPDF 内部 C PNG encoder，性能被 PyMuPDF 包装层严重拖累
- 对比：PIL PNG 编码 p50 14ms，Rust `image` crate 更快
- **光这一项就吃掉 fitz 一半总耗时**

**2. 纯光栅化对比 MuPDF 也不最快**
- fitz (MuPDF) raster：4.88s
- pypdfium2 (PDFium) raster：2.91s ← **PDFium 反而更快**
- MuPDF 历史更久，但 PDFium 有 Chrome 团队多年优化，特别是在病理 PDF 上更鲁棒

**3. flashpdf 的优势来源**（按贡献排序）
- 用 Rust `image` crate 编码 PNG：比 PIL 快、比 MuPDF PNG encoder 快得多
- PDFium raster 本身就快
- 没有 PIL 中转的字节拷贝（pypdfium2 的 `bitmap.to_pil()` 多一次拷贝）
- `Box::leak` 全进程缓存 PDFium 实例，避免每次 `FPDF_InitLibrary`

**4. 对 flashpdf 不利但仍然领先的一点**
- `flashpdf.open()` 是 eager 全文档文本提取（corpus 总耗时 0.46s）
- fitz.open() / pypdfium2.PdfDocument() 是 lazy（0.04-0.07s）
- 也就是说 flashpdf 在做"无用功"（用户只想要渲染时文本提取是浪费）的情况下仍然领先
- 未来若加 `render_only` 模式跳过文本提取，差距还会再拉开 ~0.4s

## "fitz 应该最快"的直觉为什么在这里不成立

| 场景 | fitz 是不是最快？ | 原因 |
|---|---|---|
| **纯文本提取** | 不是 | README benchmark 显示 fitz 平均 15ms，flashpdf 2ms（flashpdf 自研解析器直接吃 mmap，无 PDF interpreter 开销） |
| **纯光栅化**（不编码）| 不是 | MuPDF raster 4.88s vs PDFium raster 2.91s |
| **渲染 + PNG 编码**（用户实际场景）| 不是 | MuPDF PNG encoder 性能差，p50 27ms |
| **fitz 的强项**：返回 Pixmap 对象本身（不调 `tobytes`）| 较强 | fitz open+raster 4.95s，但仍输给 PDFium 的 2.91s |

简单说：**MuPDF 是好引擎，但 PyMuPDF 的 Python 层 + 内置 PNG encoder 是短板**。
flashpdf 通过 PDFium（raster）+ Rust `image` crate（PNG encode）的组合恰好钻了这个空子。

## 复现

```bash
# 1. 下载 PDFium binary（macOS arm64 示例）
curl -L -o /tmp/pdfium.tgz \
  https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/7906/pdfium-mac-arm64.tgz
mkdir -p /tmp/pdfium && tar xzf /tmp/pdfium.tgz -C /tmp/pdfium
export PDFIUM_PATH=/tmp/pdfium/lib/libpdfium.dylib

# 2. 构建 flashpdf（带 render feature）
git clone https://github.com/justcodew/flashpdf.git
cd flashpdf && pip install maturin pillow pypdfium2 pymupdf
maturin develop --release --features render

# 3. 跑对比（需要 PyMuPDF 测试集）
git clone --depth 1 https://github.com/pymupdf/PyMuPDF.git /tmp/pymupdf
# 测试脚本见本 commit 配套的 /tmp/bench_render_v2.py
# （端到端对比）和 /tmp/bench_render_breakdown.py（阶段拆分）
```

## 注意事项

- **测试为单次跑**：没有重复 N 次取最小值。渲染 benchmark 不像文本提取那么稳定，
  但 154 个 PDF 的 corpus-level 总耗时（3.56s / 5.81s / 10.34s）相对稳健
- **三家引擎顺序跑**（flashpdf → fitz → pypdfium2）：每跑完一家 OS 文件缓存已经热，
  后跑的占便宜。flashpdf 第一个跑反而是冷缓存，仍然领先说明结果稳健
- **只测第 0 页**：完整渲染所有页内存吃不消（每页 ~1-2MB PNG × 165 文件），
  也偏离典型用例
- **150 DPI**：屏幕预览级别。300 DPI（打印）绝对数字会上升但相对顺序应保持
- **失败 PDF 是 0 页文档**（widgettest.pdf 等）：`IndexError: page index 0 out of range`，
  三家都在 `doc[0]` / `pdf[0]` 时失败，与渲染引擎无关
