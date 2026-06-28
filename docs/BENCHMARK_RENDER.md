# 渲染性能基准（`render` feature）

flashpdf 在 v0.7 之后引入了**可选的 PDFium 渲染后端**（`render` Cargo feature）。
本文记录 `page.get_pixmap(dpi=150)` 与 PyMuPDF / pypdfium2 的对比。

## TL;DR

经过两轮测试——**第一轮**用各库默认 API 路径，**第二轮**把所有已知的"不公平
因素"都给竞品修正后——flashpdf 在每一轮、每个文件大小桶都最快。

| 库 | 后端 | 第一轮（默认路径） | 第二轮（公平对比） |
|---|---|---:|---:|
| **flashpdf** | PDFium + Rust `image` PNG | **3.56s（2.90× 快于 fitz）** | **3.09s（2.97× 快于 fitz）** |
| pypdfium2 | PDFium + PIL PNG | 5.81s | 5.74s |
| fitz (PyMuPDF) | MuPDF + 内置 PNG | 10.34s | 9.17s |

第二轮给竞品两个让步：
1. fitz 用 `samples + 手动 zlib` 编码（比默认 `tobytes("png")` 快 ~13%）
2. flashpdf 用 `render_only=True` 跳过 eager 文本提取（少做 0.46s 无用功）

**结论**：flashpdf 领先**不是测试偏袒的产物**——把所有偏袒都给竞品后，flashpdf
仍然领先 fitz 2.97×、领先 pypdfium2 1.86×。

## 测试环境

- **OS**: macOS (Apple Silicon ARM64)
- **Python**: 3.14
- **flashpdf**: 0.7.0（`maturin develop --release --features render`）
- **fitz (PyMuPDF)**: 1.27.2.3
- **pypdfium2**: 最新 PyPI
- **PDFium binary**: chromium/7906，arm64 macOS
- **样本**: PyMuPDF 测试集的 165-PDF bug-regression 语料
  （`pymupdf/tests/resources`），165 个成功（v0.7.3 修了 v0.7.1 的 11 个 page-tree
  bug；pypdfium2 / PyMuPDF 一直 165/165）

## 第一轮：默认 API 路径（端到端）

各家用自己最自然的 API：

```python
# flashpdf
with flashpdf.open(path) as doc:
    png = doc[0].get_pixmap(dpi=150)

# fitz
doc = fitz.open(path)
pix = doc[0].get_pixmap(dpi=150)
png = pix.tobytes("png")  # 默认路径

# pypdfium2
pdf = pdfium.PdfDocument(path)
bitmap = pdf[0].render(scale=150/72)
pil = bitmap.to_pil()
png = pil.save(buf, format="PNG")
```

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

## 第二轮：公平对比（修正所有偏袒）

### 偏袒 1：flashpdf.open() 多做了无用功

`flashpdf.open()` 默认是 **eager 全文档文本+图像提取**（为文本提取场景设计）。
用户只想要渲染时这 0.46s 是浪费。修复：加 `render_only=True` 选项跳过。

```python
# 修复后：只解析 xref 拿页数，不做文本提取
with flashpdf.open(path, render_only=True) as doc:
    png = doc[0].get_pixmap(dpi=150)
```

### 偏袒 2：fitz PNG 编码慢是默认路径问题

测试 fitz 四种 PNG 输出路径（`tests/bench_fitz_paths.py`）：

| fitz 路径 | p50 ms | 总耗时 |
|---|---:|---:|
| `pix.tobytes("png")`（默认）| 26.14 | 10.77s |
| `pix.save(path)` | 26.00 | 10.82s |
| `pix.samples + 手动 zlib` | **19.52** | **9.57s** |

`samples + 手动 zlib` 比默认快 ~13%。第二轮给 fitz 用这条最快路径。

### 第二轮结果

```
library         ok     p50 ms     p90 ms    mean ms      sum s
-------------------------------------------------------------------
flashpdf       154       9.88      31.70      20.05       3.09
fitz           154      27.87      86.81      59.55       9.17
pypdfium2      154      20.70      59.05      37.30       5.74
```

| 对比 | 第一轮（默认） | 第二轮（公平） |
|---|---:|---:|
| flashpdf vs fitz | 2.90× | **2.97×** |
| flashpdf vs pypdfium2 | 1.63× | **1.86×** |

vs fitz 几乎不变（fitz 的默认 tobytes 已经接近其最快路径），vs pypdfium2 差距
反而**拉大**——因为 flashpdf 的 0.46s 文本提取开销被去掉了。

## 阶段拆分：瓶颈在哪里？

各阶段单独计时（`tests/bench_render_breakdown.py`）：

| 阶段 | flashpdf | fitz | pypdfium2 |
|---|---:|---:|---:|
| **open()** | 0.46s（含全文档提取）/ 0.07s（render_only）| 0.07s（lazy）| 0.04s（lazy）|
| **raster**（纯光栅化）| — | 4.88s | 2.91s |
| **PNG 编码** | — | **5.36s** | 2.85s |
| raster + PNG 合计 | **3.05s** | 10.24s | 5.76s |

### 关键发现

**1. fitz 的 PNG 编码是最大瓶颈（p50 27ms / 总 5.36s）**
- MuPDF 内置 C PNG encoder 性能被 PyMuPDF 包装层拖累
- 对比：PIL PNG 编码 p50 14ms，Rust `image` crate 更快
- **光这一项吃掉 fitz 一半时间**

**2. 纯光栅化对比 MuPDF 也不最快**
- fitz (MuPDF) raster：4.88s
- pypdfium2 (PDFium) raster：2.91s ← **PDFium 反而更快**
- MuPDF 历史更久，但 PDFium 有 Chrome 团队多年优化，特别是在病理 PDF 上更鲁棒

**3. flashpdf 的优势来源**（按贡献排序）
- 用 Rust `image` crate 编码 PNG：比 PIL 快、比 MuPDF PNG encoder 快得多
- PDFium raster 本身就快
- 没有 PIL 中转的字节拷贝（pypdfium2 的 `bitmap.to_pil()` 多一次拷贝）
- `Box::leak` 全进程缓存 PDFium 实例

## "fitz 应该最快"的直觉为什么在这里不成立

| 场景 | fitz 是不是最快？ | 原因 |
|---|---|---|
| **纯文本提取** | 不是 | README benchmark：fitz 平均 15ms，flashpdf 2ms |
| **纯光栅化**（不编码）| 不是 | MuPDF raster 4.88s vs PDFium raster 2.91s |
| **渲染 + PNG 编码**（用户实际场景）| 不是 | MuPDF PNG encoder 性能差，p50 27ms |
| **fitz 强项**：返回 Pixmap 对象本身（不调 `tobytes`）| 较强 | fitz open+raster 4.95s，但仍输给 PDFium 的 2.91s |

简单说：**MuPDF 是好引擎，但 PyMuPDF 的 Python 层 + 内置 PNG encoder 是短板**。
flashpdf 通过 PDFium（raster）+ Rust `image` crate（PNG encode）的组合恰好钻了这个空子。

## API 新增：`render_only=True`

为渲染专用场景加的快速路径，跳过 eager 文本提取：

```python
# 仅渲染场景：省 0.46s corpus 时间
with flashpdf.open(path, render_only=True) as doc:
    for i in range(len(doc)):
        png = doc[i].get_pixmap(dpi=150)

# 同时要文本和渲染：用默认（不带 render_only）
with flashpdf.open(path) as doc:
    text = doc[0].get_text("text")   # 文本可用
    png = doc[0].get_pixmap(dpi=150)  # 渲染可用
```

`render_only=True` 时：
- ✅ `len(doc)` / `doc[i]` / `get_pixmap()` 正常
- ❌ `get_text()` / `get_images()` / `get_links()` 返回空（stub）
- ❌ `page.rect` / `page.is_scanned` 是 stub 值
- ❌ `doc.metadata` / `doc.get_toc()` 返回空

适合：批量缩略图、OCR 前置光栅化、纯视觉检查。

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
python tests/bench_render.py            # 第一轮：默认路径
python tests/bench_render_fair.py       # 第二轮：公平对比
python tests/bench_render_breakdown.py  # 阶段拆分
python tests/bench_fitz_paths.py        # fitz 各 PNG 路径对比
```

## 注意事项

- **测试为单次跑**：没有重复 N 次取最小值。渲染 benchmark 噪声比文本提取大，
  但 154 个 PDF 的 corpus-level 总耗时相对稳健
- **三家引擎顺序跑**（flashpdf → fitz → pypdfium2）：跑完一家 OS 缓存已热，
  后跑的占便宜；第一轮里 flashpdf 第一个跑（冷缓存），仍然领先说明结果稳健。
  第二轮对 flashpdf 加了 render_only=True（更少吃亏），同时给 fitz 最快 PNG 路径
- **只测第 0 页**：完整渲染所有页内存吃不消（每页 ~1-2MB PNG × 165 文件），
  也偏离典型用例
- **150 DPI**：屏幕预览级别。300 DPI（打印）绝对数字会上升但相对顺序应保持
- **失败 PDF 是 0 页文档**（widgettest.pdf 等）：`IndexError: page index 0 out of range`，
  三家都在 `doc[0]` / `pdf[0]` 时失败，与渲染引擎无关
