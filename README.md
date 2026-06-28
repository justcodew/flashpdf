# flashpdf

**文本提取 + 页面渲染速度最快的 PDF 库**。Rust 核心 + Python 绑定。

> 在 165-PDF PyMuPDF bug-regression 病理语料上：
> - 文本提取比第二名快 **11×**（flashpdf 3.06ms vs pdf_oxide 34ms）
> - 页面渲染比第二名快 **1.4×**（flashpdf 19.72ms vs liteparse 27.80ms，165/165 零失败）
> - 所有文件大小桶（tiny / small / medium / large）都是第一
>
> **速度领先 ≠ 全维度领先**——加密 PDF（AES-256）、编辑、注释、表单、OCR、表格提取
> 等场景仍推荐 PyMuPDF / pdf_oxide / pypdf / pdfplumber（按场景选）。完整对比见
> [BENCHMARK_FULL.md](docs/BENCHMARK_FULL.md)，已知短板见
> [LIMITATIONS.md](docs/LIMITATIONS.md)。

## 提取效果演示

[完整交互页](docs/demo.html) · 样例：arxiv 双栏学术论文首页（标题 + 摘要 + 双栏正文）

![提取效果](docs/demo.png)

flashpdf 正确识别标题 / 作者行 / abstract / 双栏正文为独立 block，字号信息保留，
阅读顺序对齐 PDF 视觉顺序。

## 安装

```bash
pip install flashpdf
```

源码构建（需要 [Rust 工具链](https://rustup.rs)）：

```bash
git clone https://github.com/justcodew/flashpdf.git
cd flashpdf && pip install maturin && maturin develop --release
```

## 快速开始

```python
import flashpdf

# fitz 风格（推荐）：open + per-page 查询
with flashpdf.open("paper.pdf") as doc:
    print(len(doc))                  # 页数
    page = doc[0]                    # 首页（支持 doc[-1] 负索引）
    d  = page.get_text("dict")       # 结构化 {blocks:[...]}，文本块 type=0、图像块 type=1 内联
    t  = page.get_text("text")       # 纯文本拼接
    bs = page.get_text("blocks")     # fitz "blocks" 元组列表
    imgs = page.get_images()         # 该页嵌入图像
    print(page.is_scanned, page.rect, page.number)

# 函数式批量（处理大量 PDF 的最高吞吐入口）
for path, blocks, images in flashpdf.extract_many(
    ["a.pdf", "b.pdf", "c.pdf"],
    file_parallel=True,
):
    ...
```

### 命令行（`flashpdf`）

```bash
# 提取纯文本（默认模式，stdout）
flashpdf extract paper.pdf

# fitz 风格 JSON，并写入文件
flashpdf extract paper.pdf --mode dict --pages 0,1,5-8 --output-dir out/

# 元数据 + 页数概览
flashpdf info paper.pdf
flashpdf info paper.pdf --per-page      # 每页 is_scanned / 块数

# 目录（outline / TOC）
flashpdf toc paper.pdf                  # 树状缩进格式
flashpdf toc paper.pdf --rich           # 完整 JSON（含 kind/uri/to_point）
```

`flashpdf` 命令随 `pip install` 自动注册（基于 click，[project.scripts] 入口）。


## 特性

- **极致性能**：mmap 零拷贝、memchr SIMD 扫描、rayon 页级并行
- **完整解码链路**：CMap、Type0 CIDFont、Encoding Differences、嵌入式 Type1 字体 /Encoding、Adobe Glyph List
- **健壮容错**：xref 损坏时全文扫描恢复；165-PDF 病理语料 **0% 失败率**
- **fitz 兼容 API**（v0.2.0+）：`open()` / `Document` / `Page.get_text("dict"|"text"|"blocks")` 与 PyMuPDF 常用接口一一对应
- **图像提取**：嵌入位图（JPEG/PNG/JPX）零拷贝直传，保留原始字节与四角变换 bbox

## 适用范围

flashpdf 是**纯只读数据提取 + 可选页面渲染工具**——速度快是核心目标，但不是全场景方案。

### ✅ 擅长（速度第一）

- **文本提取**：blocks/lines/spans，含 bbox/字体/字号/颜色；165-PDF 语料 0% 失败率
- **嵌入图像提取**：`Do` 引用的位图（JPEG/PNG/JPX）+ BI/ID/EI 内联图像，原始字节直出
- **页面渲染**（需 `render` feature + PDFium binary）：`page.get_pixmap(dpi=150)` 返回 PNG bytes
- **批量吞吐**：mmap 零拷贝、memchr SIMD 扫描、rayon 页级并行 + 文件级并行

### ❌ 不做（设计目标，永远不会做）

- **OCR**：不识别扫描页文字，只识别"是扫描页"；要 OCR 用 Tesseract / PaddleOCR
- **PDF 编辑**：合并/拆分/加页/删页/表单填写/签名/注释——用 PyMuPDF / pypdf / pdf_oxide
- **AES-256 加密 PDF**：只支持 RC4 + AES-128 空密码；AES-256 直接抛 `ValueError`
- **矢量路径数据提取**：text-extraction 核心不解析 path 算子（曲线/填充/裁剪）；
  PDFium 渲染时会把矢量图完整光栅化进 PNG（`get_pixmap()` 输出包含全部矢量内容），
  但不暴露 path 坐标/曲线参数。要 vector path 数据用 PyMuPDF 的 `page.get_drawings()`
- **可访问性标记树（/StructTree）/ 嵌入式文件流 / 增量更新解析**

### ⚠️ 能做但不是最强（推荐看场景换库）

| 场景 | flashpdf 表现 | 更合适的库 |
|---|---|---|
| 加密 PDF（任意密码 / AES-256）| 不支持 | PyMuPDF / pypdfium2 |
| PDF 编辑（合并 / 拆分 / 签名 / 水印）| 不支持 | pdf_oxide / PyMuPDF / pypdf |
| 渲染部署便利（pip 装即用）| 需 PDFium binary | pypdfium2（自带 PDFium）|
| 渲染输出 raw RGBA / numpy | 只输出 PNG bytes | PyMuPDF（`pix.samples` 直出）|
| `span.flags` 精度 | 名字启发式（不读 /FontDescriptor /Flags）| PyMuPDF |
| 字体度量字段（ascender/descender/origin）| 不输出 | PyMuPDF |
| 表格提取（精确 cell 坐标）| 不做 | 规则派（pdfplumber / pdftext，简单有线框表格）／ 模型派（Surya / PaddleOCR PP-Structure / Table Transformer，复杂 / 无边框 / 合并单元格）¹ |
| LLM 友好 markdown 输出 | 不做 | markitdown / pdftext |

¹ 表格提取在 Python 生态**没银弹**：规则派 50-90% 准确率（看表格复杂度），
模型派 90%+ 但需 GPU / ONNX runtime。flashpdf 不做的真实理由不是"懒得做"而是
"做不好且别人也没做好"——表格识别是 layout 分析层的独立子领域。详见
[BENCHMARK_FULL.md §5](docs/BENCHMARK_FULL.md)。

完整短板清单（加密限制细节、字段精度、未测场景、渲染 API 边界等）见
**[LIMITATIONS.md](docs/LIMITATIONS.md)**；10 库全面对比见
**[BENCHMARK_FULL.md](docs/BENCHMARK_FULL.md)**；纯渲染基准见
**[BENCHMARK_RENDER.md](docs/BENCHMARK_RENDER.md)**。

## 基准

> **新**：10 库全面对比（文本提取 + 页面渲染 + 分桶 + 选型建议）见
> **[BENCHMARK_FULL.md](docs/BENCHMARK_FULL.md)**。下面是文本提取的快速摘要。

**165-PDF 病理语料**（PyMuPDF bug-regression 测试集，每个 PDF 是一次历史 bug 的最小复现，
覆盖 CJK / 扫描 / 加密 / 表格 / 表单 / 矢量图，865B-8.3MB）：

| 库 | 成功率 | mean | p50 | p90 | 失败 |
|---|---:|---:|---:|---:|---:|
| **flashpdf** | **165/165** | **2.05ms** | **0.36ms** | **4.31ms** | **0** |
| pdf_oxide | 163/165 | 7.79ms | 1.14ms | 15.25ms | 2 (`2.pdf`, `joined.pdf`) |
| liteparse | 164/165 | 13.89ms | 1.52ms | 27.08ms | 1 (hang on `circular-toc.pdf`) |
| fitz (PyMuPDF) | 165/165 | 15.20ms | 1.66ms | 33.50ms | 0 |

**全文提取总耗时（165 文件累加）**：flashpdf **0.34s** vs pdf_oxide 1.27s vs liteparse 2.28s vs fitz 2.51s。

**速度倍数（corpus 总耗时）**：vs pdf_oxide **3.76×**、vs liteparse **6.74×**、vs fitz **7.42×**。

**按文件大小分桶（p50 ms）**：

| 桶 | n | flashpdf | pdf_oxide | liteparse | fitz |
|---|---:|---:|---:|---:|---:|
| tiny <10KB | 32 | **0.092** | 0.093 | 0.252 | 0.618 |
| small 10-100KB | 50 | **0.310** | 0.776 | 0.938 | 1.485 |
| medium 100KB-1MB | 63 | **0.888** | 2.868 | 4.250 | 5.584 |
| **large >1MB** | 20 | **4.352** | 19.667 | 27.780 | 22.446 |

**结论**：flashpdf 在**每一个大小桶都是最快**——包括 tiny 文件（与 pdf_oxide 持平）。
优势随文件大小放大：tiny 桶 ~1×，large 桶 **4.5-6.4×**。RAG 索引、批量预处理、
大文档解析等"重负载"场景 flashpdf 是首选；发票/邮件附件等小文件批量场景优势最小但不落下风。

单文件重负载场景（14-15 页 arxiv 论文 + rayon 多核加速）下 flashpdf 还可达 5-12× 领先，
这是最佳场景而非平均，详见 [BENCHMARK.md](docs/BENCHMARK.md)（含 v0.1.3 vs 10 个主流
Python PDF 库横评、v0.1.x → v0.3.x 稳定性演进、字符级精度对比）。

**复现**：

```bash
git clone --depth 1 https://github.com/pymupdf/PyMuPDF.git /tmp/pymupdf
pip install flashpdf liteparse pdf-oxide pymupdf
# 跑对比（liteparse 在 circular-toc.pdf 上无限循环，已在脚本里跳过）
CORPUS_DIR=/tmp/pymupdf/tests/resources python tests/bench_corpus.py
```

## API 参考

### `flashpdf.open(path, **options) -> Document`

fitz 风格入口。open() 时一次性并行提取所有页，后续 `doc[i]` / `get_text()` 纯内存查询。

**Page 方法/属性**：

| API | 说明 |
|---|---|
| `page.get_text(mode)` | `"dict"`（默认）/`"text"`/`"blocks"`，与 fitz 对齐 |
| `page.get_images()` | 该页所有嵌入图像列表 |
| `page.is_scanned: bool` | 扫描页启发式（v0.1.4） |
| `page.rect: [x0,y0,x1,y1]` | MediaBox |
| `page.number: int` | 0-based 页码 |
| `page.diagnostics: dict` | 见 [进阶](#进阶) |

**主要选项**：

| 参数 | 默认 | 说明 |
|---|---|---|
| `include_images` | `True` | 是否提取图像字节（纯文本场景设 `False` 省解码时间） |
| `include_rotated` | `False` | 是否提取旋转/侧排文本（arXiv 侧栏水印、图表纵轴标签） |
| `page_parallel` | `True` | 页级并行 |

### `flashpdf.extract(path, **options) -> (blocks, images[, pages])`

函数式单文件提取。设 `with_page_info=True` 多返回一个 `pages` 列表（含 `is_scanned`）。

### `flashpdf.extract_many(paths, **options) -> Iterator`

批量提取，`file_parallel=True` 默认开启。

### blocks / images 结构

```python
# blocks：文本块（open() 的 dict 模式下图像块 type=1 内联到同一数组）
{
    "type": 0,                       # 0=文本，1=图像
    "bbox": (x0, y0, x1, y1),
    "lines": [{"bbox": ..., "spans": [
        {"bbox": ..., "text": "...", "font": "Helvetica",
         "size": 12.0, "color": 0, "flags": 0}   # flags: 名字启发式 italic/serif/mono/bold
    ]}]
}

# images：嵌入位图（Do 引用）
{
    "bbox": (x0, y0, x1, y1),
    "width": 1920, "height": 1080,
    "colorspace": "DeviceRGB", "bpc": 8,
    "ext": "jpeg",                   # jpeg/png/jpx
    "image": b"\xff\xd8...",         # 原始字节
}
```

**fitz 兼容性**：`open()` / `doc[i]` / `get_text("dict"|"text"|"blocks")` / `page.rect` /
`page.get_images()` 全部对齐。不支持编辑类 API（设计目标，详见 [LIMITATIONS.md](docs/LIMITATIONS.md)）。
`span.flags` 通过字体名启发式检测 italic/serif/mono/bold（不读 `/FontDescriptor /Flags`，
精度不如 fitz）；`ascender/descender/origin` 等 fitz 扩展字段不输出。

**何时用哪个**：交互式 / 逐页随机访问 → `open()`；批量向量化 → `extract_many(file_parallel=True)`；
一次性单文件 → `extract()`。

## 进阶

### 扫描页检测（`is_scanned`）

flashpdf 不做 OCR，但能识别扫描页（启发式：页内可提取字符 < 50 且存在覆盖页面 ≥ 70% 的位图）。
混合文档按页分别判断：

```python
with flashpdf.open("mixed.pdf") as doc:
    for i in range(len(doc)):
        page = doc[i]
        if page.is_scanned:
            for img in page.get_images():
                your_ocr(img["image"])
        else:
            print(page.get_text("text"))
```

### 旋转文本提取（`include_rotated`）

PDF 里 90°/270° 旋转的字符（arXiv 侧栏水印、图表纵轴标签）默认丢弃——避免污染 XY-cut
阅读序算法。需要的话 `open(path, include_rotated=True)`，旋转字符会作为独立 block 追加到
页末尾（不参与 XY-cut 排序，正文字符提取字节级不变）。

### 诊断信息（`page.diagnostics`）

每页暴露 4 个计数器，告诉你"N 个字符被丢弃"，决定是否重提取或交 OCR。检测总是发生，
即使对应开关是关的：

| 字段 | 含义 | 触发后的处理建议 |
|---|---|---|
| `rotated_char_count` | 非轴对齐文本矩阵下的字符 | 用 `include_rotated=True` 重提取 |
| `type3_char_count` | Type3 字体下的字符 | 检查是否需要专门的 Type 3 处理器或 OCR |
| `undecoded_byte_count` | 解码失败回退为 U+FFFD 的字节数 | 多为字体子集化遗留，OCR 能补回 |
| `out_of_page_block_count` | reading-order 边距过滤器丢弃的块 | 多为矢量图误聚或旋转文本越界 |
| `inline_image_count` | BI/ID/EI 操作符嵌入的内联图像数 | 老扫描 PDF / 票据 / Office 导出常见，会进入 `page.get_images()`（`name="inline"`） |

### 多线程策略（`page_parallel`）

| 模式 | 适用场景 | 说明 |
|---|---|---|
| **MT**（`page_parallel=True`，默认） | 单文件提取 | rayon 把各页并行到多核，14-15 页重负载 3-4× 加速 |
| **ST**（`page_parallel=False`） | `extract_many` 批量 | 与 `file_parallel=True` 配合避免 rayon 嵌套 |

> 所有对比库（pdf_oxide / PyMuPDF / pypdfium2 等）都是单线程跑的，**flashpdf-ST 才是
> apples-to-apples 的对比**（仍然比所有其他库快），MT 是 flashpdf 额外的多核加成。

## 架构

```
PDF ─ mmap 零拷贝
   ├─ 自研解析器（对象 / xref 表+流+ObjStm + memchr 损坏恢复）
   ├─ 内容流状态机（BT/ET, Tj/TJ, Td/Tm, Form XObject 递归）
   ├─ 字体（CMap, Type0 CIDFont, Encoding, Adobe Glyph List）
   ├─ 布局（chars → spans → lines → blocks）
   └─ 图像（JPEG/JPX 零拷贝，FlateDecode 惰性 PNG，四角变换 bbox）
并行：rayon 页级 + 文件级 + 异步预读 + 大文档分批
```

设计文档 [DESIGN_V1](docs/DESIGN_V1.md) / [DESIGN_V2](docs/DESIGN_V2.md)；
完整 API 详情 [API.md](docs/API.md)。

## 测试

```bash
cargo test -p flashpdf-core    # 39 个核心单元测试
cargo bench -p flashpdf-core   # 性能基准
```

## 依赖

`memchr`（SIMD 扫描）· `flate2`（zlib）· `memmap2`（mmap）· `rayon`（并行）·
`pyo3`（Python 绑定）· `fast-float2` · `crc32fast` · `fnv` · `smallvec`

## 路线图

- [x] 自研解析器 / 内容流 / 字体 / 布局 / 图像 / 并行化 / PyPI 发布 + CI/CD
- [x] **v0.4.0** fitz 功能补全：`span.flags` · TOC · 链接 API · CLI
- [x] **v0.5.0** 适用面扩大：加密 PDF · 错误信息 · examples · 迁移指南
- [x] **v0.6.0** 精度深挖：Type3 · 竖排文本 · char_sim 残差
- [x] **v0.7.0** 规模化：~~扩语料~~（跳过） · tiny 性能 · logging · PERFORMANCE.md

详见 [docs/ROADMAP.md](docs/ROADMAP.md)。

## 许可证

MIT
