# flashpdf

世界上最快的 PDF 文本与图像提取引擎。

Rust 核心 + Python 绑定，输出与 PyMuPDF 兼容的 `blocks` 和 `images` 结构。

## 提取效果演示

左侧是 fitz 渲染的原始 PDF 页面，右侧是 flashpdf `get_text("dict")` 的输出结构
（每个色块是一个 block，按 line 展开，标注字号便于核对排版）。

**👉 完整交互页：[docs/demo.html](docs/demo.html)**（自包含单文件，离线可看）

![提取效果](docs/demo.png)

上图样例：arxiv 双栏学术论文首页（标题 + 摘要 + 双栏正文）。可见 flashpdf：
- 正确识别标题、作者行、abstract、双栏正文为独立 block
- 字号信息（title 大字号、body 小字号）保留完整
- 阅读顺序对齐 PDF 视觉顺序（v0.1.3 阅读顺序优化的成果）

> 想用别的 PDF 重新生成？`python tests/gen_demo.py` 即可（脚本内可改 PDF 路径和页码）。

## 特性

- **极致性能**：全链路零拷贝 (mmap)、SIMD 字节扫描 (`memchr`)、快速浮点解析 (`fast-float`)
- **不牺牲信息**：完整的文本提取链路，包括 CMap、Type0 复合字体、Form XObject 递归
- **并行处理**：rayon 页级并行 + 文件级并行 + 异步预读
- **健壮容错**：xref 损坏时自动 memchr 全文扫描恢复
- **fitz 兼容 API**（v0.2.0+）：`flashpdf.open(path)` → `Document` → `doc[i]` →
  `page.get_text("dict"|"text"|"blocks")`，与 PyMuPDF 的常用接口一一对应；
  原 `extract()` / `extract_many()` 保留用于批量场景

## 适用范围（务必先读）

flashpdf 是**纯数据提取工具**，不是 PDF 渲染器。明确边界：

- ✅ **文本提取**：按页面顺序输出结构化 blocks/lines/spans（含 bbox、字体、字号、颜色）
- ✅ **嵌入图像提取**：抽取 PDF 内部以 `Do` 操作符引用的位图对象
  （JPEG/PNG/JPX），保留**原始字节**与四角变换 bbox——即"图片对象"，
  **不是**"页面截图"
- ❌ **不支持页面渲染**：不能把整页渲染成位图（`Page.get_pixmap()` 等价物）
- ❌ **不支持内容重建**：不会把矢量图、路径、文字光栅化为图像
- ❌ **不做 OCR**：扫描页上的图文字无法直接转为文本，但能给出
  `is_scanned` 标记 + 原始图像字节，方便你自己接 OCR 引擎

如果你的需求是"得到每页的 PNG 预览图"或"扫描件 OCR 前的位图输入"，
请使用 PyMuPDF / ritz / GoMuPDF 等带渲染引擎的库——渲染需要完整的
PDF interpreter + 光栅化器（MuPDF C 库），这与 flashpdf "纯解析、零渲染"
的设计目标相悖。

### 扫描页检测

flashpdf **不做 OCR**，但可以**识别**哪些页是扫描的，方便你只对那些页
调用外部 OCR（Tesseract / PaddleOCR / 云 OCR）。启发式：页内可提取
文本字符 < 50 且存在覆盖页面 ≥ 70% 的位图。

```python
import flashpdf

# 默认 (blocks, images) — 向后兼容
blocks, images = flashpdf.extract("doc.pdf")

# 加 with_page_info=True 拿 per-page 元数据
blocks, images, pages = flashpdf.extract("doc.pdf", with_page_info=True)

for p in pages:
    if p["is_scanned"]:
        # 这一页是扫描的，没有可提取文本
        # 你可以从 images 里找该页的大图，喂给 OCR
        page_imgs = [i for i in images if page_bbox_overlap(i, p)]
        print(f"Page {p['page']}: SCANNED, {len(page_imgs)} image(s) need OCR")
    else:
        print(f"Page {p['page']}: electronic text")
```

对混合文档（部分电子 + 部分扫描）同样有效——按页分别判断。

## 安装

```bash
pip install flashpdf
```

从源码构建：

```bash
# 需要 Rust 工具链 (https://rustup.rs)
git clone https://github.com/yourname/flashpdf.git
cd flashpdf
pip install maturin
maturin develop --release
```

## 快速开始

### Python

```python
import flashpdf

# 单文档提取
blocks, images = flashpdf.extract("document.pdf")

for block in blocks:
    for line in block["lines"]:
        for span in line["spans"]:
            print(f"[{span['font']} {span['size']:.0f}] {span['text']}")

for img in images:
    print(f"Image: {img['width']}x{img['height']} {img['ext']}")
    # img['image'] 是原始字节 (JPEG/PNG)

# 批量提取 (文件级并行)
for path, blocks, images in flashpdf.extract_many(
    ["a.pdf", "b.pdf", "c.pdf"],
    file_parallel=True,
    include_images=False
):
    print(f"{path}: {len(blocks)} blocks")
```

### Rust

```rust
use flashpdf_core::{extract, ExtractOptions};

let options = ExtractOptions {
    page_parallel: true,
    include_images: true,
    batch_size: 50,
    ..Default::default()
};

let result = extract("document.pdf", &options)?;

for page in &result.pages {
    for block in &page.blocks {
        for line in &block.lines {
            for span in &line.spans {
                println!("[{} {:.0}] {}", span.font, span.size, span.text);
            }
        }
    }
}
```

### fitz 风格 API（v0.2.0+）

`flashpdf.open()` 提供与 PyMuPDF 一一对应的 Document / Page 接口。**open() 时
一次性并行提取所有页**，后续 `doc[i]` / `get_text()` 纯内存查询，零延迟。

```python
import flashpdf

with flashpdf.open("paper.pdf") as doc:
    print(len(doc))                  # 页数
    print(doc.page_count)            # 同上，fitz 风格

    page = doc[0]                    # 首页（支持 doc[-1] 负索引）

    # 三种 get_text 模式，与 fitz.Page.get_text 对齐
    d  = page.get_text("dict")       # 结构化 {blocks:[...]}，文本块 type=0，图像块 type=1 内联
    t  = page.get_text("text")       # 纯文本拼接
    bs = page.get_text("blocks")     # fitz "blocks" 模式 (x0,y0,x1,y1,text,no,type)

    # 其他属性
    print(page.is_scanned)           # 该页是否扫描页（v0.1.4 引入的启发式）
    print(page.rect)                 # MediaBox [x0,y0,x1,y1]
    print(page.number)               # 0-based 页码

    imgs = page.get_images()         # 该页的图像列表（每个含 bbox/width/height/ext/image bytes）

# 按页处理混合文档
with flashpdf.open("mixed.pdf") as doc:
    for i in range(len(doc)):
        page = doc[i]
        if page.is_scanned:
            # 扫描页：拿图像字节喂给 OCR
            for img in page.get_images():
                your_ocr(img["image"])
        else:
            # 电子页：直接用文本
            print(page.get_text("text"))
```

**与 fitz 的对照**：

| fitz API | flashpdf API | 状态 |
|----------|--------------|------|
| `fitz.open(path)` | `flashpdf.open(path)` | ✅ |
| `len(doc)` / `doc.page_count` | 同左 | ✅ |
| `doc[i]` / `doc[-1]` | 同左 | ✅ |
| `page.get_text("dict")` | 同左 | ✅（span 含 `flags=0` stub，下表说明） |
| `page.get_text("text")` | 同左 | ✅ |
| `page.get_text("blocks")` | 同左 | ✅ |
| `page.rect` / `page.number` | 同左 | ✅ |
| `page.get_images()` | 同左 | ✅ |
| `page.get_pixmap()` | ❌ | 不支持渲染（设计目标） |
| `page.add_annotation()` 等 | ❌ | 不支持编辑 |

**span 字段对比**：

| 字段 | fitz | flashpdf | 备注 |
|------|------|----------|------|
| `bbox`, `text`, `font`, `size`, `color` | ✅ | ✅ | 完全对齐 |
| `flags` | ✅（italic/bold/serif bitmask） | `0`（stub） | v0.2.0 不带格式探测，未来增强 |
| `ascender`, `descender`, `origin`, `alpha`, `bidi`, `char_flags` | ✅ | ❌ | fitz 扩展字段，flashpdf 不输出 |

> **批量场景仍用 `extract()`**：`open()` 适合交互式/逐页访问，但若你要处理
> 1000+ 个 PDF 做向量化，`extract_many(file_parallel=True, page_parallel=False)`
> 仍是最高吞吐的入口。

#### `open()` vs `extract()` 性能与精度对比

两种 API 走同一个 Rust 核心，**字符级输出完全一致**（regression check 全部 OK）。
v0.2.1 起 `PyDocument` / `PyPage` 通过 `Arc<PageResult>` 共享所有权——`doc[i]`
只是一次原子引用计数 bump，**零深拷贝**，不再 clone blocks/images。

| 文件 | API | Mean | p99 | 字符数 | char_sim vs fitz |
|------|-----|-----:|----:|------:|-----------------:|
| dbnet_plus | `flashpdf.extract()` MT (`include_images=False`) | 5.93 ms | 8.13 ms | 56315 | 92.89% |
| dbnet_plus | `flashpdf.open(include_images=False)` | **4.87 ms** | 5.33 ms | 56315 | 92.89% |
| dbnet_plus | `flashpdf.open()` (默认提取图像) | 11.36 ms | 13.15 ms | 56315 | 92.89% |
| dbnet_plus | `fitz.open()` | 268.84 ms | 273.81 ms | 57191 | 100% |
| arxiv_2604 | `flashpdf.extract()` MT (`include_images=False`) | 8.79 ms | 9.87 ms | 59112 | 92.77% |
| arxiv_2604 | `flashpdf.open(include_images=False)` | **8.37 ms** | 8.91 ms | 59112 | 92.77% |
| arxiv_2604 | `flashpdf.open()` (默认提取图像) | 9.18 ms | 9.88 ms | 59112 | 92.77% |
| arxiv_2604 | `fitz.open()` | 144.35 ms | 153.84 ms | 60978 | 100% |

- **精度**：`open()` 与 `extract()` 字符数完全相等（同一核心，无回归）；
  char_sim 与 fitz 保持在 92-93%（fitz 多输出一些空白/控制字符，flashpdf 自动裁剪）。
- **速度**：同等条件下（都不提图像），`open()` 反而比 `extract()` **快 5-18%**——
  因为 `extract()` 在返回前要为所有页的所有 block 物化 Python dict，而 `open()` 用
  Arc 共享，只在 `get_text()` 调用时才构造 dict。即便默认开图像，仍比 `fitz.open()`
  快 **15-25x**。
- **`include_images` 参数**：纯文本场景传 `False` 可省下大量图像解码时间
  （dbnet_plus 这种多图文档能省一半）。
- **何时用哪个**：交互式 / 逐页随机访问 → `open()`；批量向量化 → `extract_many()`。

### 旋转文本提取（`include_rotated`，v0.2.0+）

PDF 里有两类文本默认**不会**出现在 `get_text()` 的输出里：

1. **侧栏水印** —— arXiv 投稿在页面左侧用 90° 旋转的 Times-Roman 渲染的
   `arXiv:2604.11578v1 [quant-ph] 13 Apr 2026` 字符串。
2. **图表纵轴标签** —— 旋转 90°/270° 的 "Width"、"Loss" 等轴标。

默认行为（`include_rotated=False`）丢弃这些字符，原因有二：

- XY-cut 阅读序算法不能在同一页混合水平和竖直块，强行纳入会把正文阅读序打乱。
- 标准 14 字体（Times-Roman、Helvetica 等）的字符宽度走 Adobe AFM 兜底，
  默认 1em 会让 40 字符的侧栏跨出页面边界、被阅读序过滤器再丢弃一次。

显式传 `include_rotated=True` 可启用：

```python
doc = flashpdf.open("arxiv.pdf", include_rotated=True)
# 侧栏文本会出现在对应页的 block 列表末尾（不参与 XY-cut）
```

启用后：

- 正文字符的提取**完全不变**（同一 TRM 路径，非旋转分支字节级保持原状）。
- 旋转字符用 TRM 4-角变换计算正确 bbox，per-char advance 在没有 `/Widths` 时
  取 0.5em 估计值（Latin 文本经验平均值），让侧栏整体落在页面边界内。
- 旋转字符被独立聚类并**追加到页 block 列表末尾**，不进入 XY-cut 排序——
  因此正文 char_sim 与 fitz 基本持平，侧栏文本作为独立块可被检索到。

代价：旋转文本在 block 内部按字符聚类成行，每个字符可能独立成行（因为
`build_lines` 假设水平流向）。对侧栏水印场景只是"读起来一字一顿"，
内容是完整可检索的；如果未来要做语义级解析，需要加竖排聚类。

## API 参考

### `flashpdf.extract(path, **options)`

从单个 PDF 文件提取文本和图像。

**参数：**

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `path` | `str` | *必填* | PDF 文件路径 |
| `page_parallel` | `bool` | `True` | 页级并行（多核加速） |
| `include_images` | `bool` | `True` | 是否提取图像数据 |
| `gpu` | `bool` | `False` | GPU 加速（需要 NVIDIA GPU） |
| `batch_size` | `int` | `50` | 大文档分批大小（0=不分批） |
| `with_page_info` | `bool` | `False` | 是否返回 per-page 元数据（含 `is_scanned`） |
| `include_rotated` | `bool` | `False` | 是否提取旋转/侧排文本（arXiv 侧栏水印、图表纵轴标签）。详见下文 |

**返回值：** `(blocks, images)`，或 `with_page_info=True` 时为 `(blocks, images, pages)`
- `pages`: `[{"page": 0, "is_scanned": False}, ...]`

#### blocks 结构

```python
[
    {
        "type": 0,                    # 0 = 文本块
        "bbox": (x0, y0, x1, y1),    # 块边界框
        "lines": [
            {
                "bbox": (x0, y0, x1, y1),
                "spans": [
                    {
                        "bbox": (x0, y0, x1, y1),
                        "text": "Hello World",
                        "font": "Helvetica",
                        "size": 12.0,
                        "color": 0,
                    }
                ]
            }
        ]
    }
]
```

#### images 结构

每个元素是 PDF 内部通过 `Do` 操作符引用的**嵌入位图对象**（JPEG/PNG/JPX），
**不是页面渲染结果**。如果你需要"页面截图"或"整页光栅化"，请改用带渲染
引擎的库（PyMuPDF / ritz / GoMuPDF）。

```python
[
    {
        "bbox": (x0, y0, x1, y1),    # 页面中的位置
        "width": 1920,                # 像素宽度
        "height": 1080,               # 像素高度
        "bpc": 8,                     # 每通道位数
        "colorspace": "DeviceRGB",    # 色彩空间
        "xref": 42,                   # 对象编号
        "ext": "jpeg",                # 格式: jpeg/png/jpx
        "image": b"\xff\xd8\xff...",   # 原始字节 (None 如果 include_images=False)
    }
]
```

### `flashpdf.extract_many(paths, **options)`

批量提取多个 PDF 文件。

**参数：**

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `paths` | `list[str]` | *必填* | PDF 文件路径列表 |
| `file_parallel` | `bool` | `True` | 文件级并行 |
| `page_parallel` | `bool` | `False` | 页级并行（与 file_parallel 互斥时建议关闭） |
| `include_images` | `bool` | `False` | 是否提取图像 |
| `gpu` | `bool` | `False` | GPU 加速 |
| `batch_size` | `int` | `50` | 大文档分批大小 |
| `with_page_info` | `bool` | `False` | 是否返回 per-page 元数据（含 `is_scanned`） |
| `include_rotated` | `bool` | `False` | 是否提取旋转/侧排文本 |

**返回值：** `[(path, blocks, images), ...]`，`with_page_info=True` 时每项为
`(path, (blocks, images, pages), ...)`

### 多线程 vs 单线程：`page_parallel` 怎么选

flashpdf 通过 `page_parallel` 开关在两种并行策略间切换：

| 模式 | `page_parallel` | 内部行为 | 适用场景 |
|------|-----------------|----------|----------|
| **MT** (Multi-Thread) | `True` | rayon 把 PDF 各**页**分发到多核并行处理 | 单文件提取（默认推荐） |
| **ST** (Single-Thread) | `False` | 顺序处理每一页 | 单核机器，或与 `extract_many` 的 `file_parallel` 配合 |

实测在 14-15 页的学术论文上，MT 比 ST 快 **3-4x**（dbnet_plus 4.93ms vs 13.80ms，
arxiv_2604 8.44ms vs 34.62ms），收益随 CPU 核数和页数线性增长。

**什么时候用哪个**：

```python
# 单文件提取：直接用默认 MT，多核白嫖
blocks, imgs = flashpdf.extract("a.pdf")

# 批量处理多文件：file_parallel + ST per file 更优
# 文件级并行比页级并行更粗粒度，调度开销更低，避免 rayon 嵌套
for path, blocks, imgs in flashpdf.extract_many(
    ["a.pdf", "b.pdf", "c.pdf"],
    file_parallel=True,
    page_parallel=False,
):
    ...
```

> **基准报告里的 MT / ST 是什么**：所有对比库（pdf_oxide / PyMuPDF / pypdfium2 等）
> 都是单线程跑的，所以 **flashpdf-ST 才是 apples-to-apples 的对比**（仍然比所有
> 其他库快 2-8x），MT 是 flashpdf 额外的多核加成。

## 架构

详见 [API 文档](docs/API.md) 获取完整的 API 参考。设计文档见 [DESIGN_V1](docs/DESIGN_V1.md) 和 [DESIGN_V2](docs/DESIGN_V2.md)。



```
PDF 文件
  │
  ├─ mmap 映射 (零拷贝)
  │
  ├─ 自研解析器 (~800 行)
  │   ├─ 对象解析 (递归下降)
  │   ├─ xref 表/流/ObjStm
  │   └─ memchr fallback (xref 损坏恢复)
  │
  ├─ 内容流状态机
  │   ├─ BT/ET 文本块
  │   ├─ Tj/TJ 文本操作符
  │   ├─ Td/TD/Tm 矩阵变换
  │   ├─ Form XObject 递归 (深度 3)
  │   └─ Do 图像捕获
  │
  ├─ 字体处理
  │   ├─ CMap 解析 (bfchar/bfrange)
  │   ├─ Type0 复合字体 (CIDFont)
  │   ├─ Encoding Differences
  │   └─ Adobe Glyph List
  │
  ├─ 布局分析
  │   └─ chars → spans → lines → blocks
  │
  ├─ 图像提取
  │   ├─ JPEG/JPX 零拷贝 (mmap 切片)
  │   ├─ FlateDecode 惰性 PNG
  │   └─ 四角变换 bbox
  │
  └─ 并行调度
      ├─ rayon 页级并行
      ├─ 文件级并行
      ├─ 异步预读
      └─ 大文档自动分批
```

## 性能目标

| 场景 | 目标 | 实际 (v0.1.3) |
|------|------|------|
| 文本提取 | ≥ PyMuPDF 2x | **~15-33x** (视 PDF 复杂度) |
| 文本 + 图像提取 | ≥ PyMuPDF 5x | **~17-33x** ✅ |
| 字符总量 | 与 PyMuPDF 接近 | 差异 <2% |
| 吞吐量 | — | 1500-2800 pages/sec |
| char-level 顺序相似度 | ≥ 90% | **95-97%** ✅（v0.1.3，与 PyMuPDF 对齐） |

> **解码准确性**：v0.1.3 修复了 TeX Computer Modern 字体的多码点 ToUnicode
> 映射、嵌入式 Type1 字体程序的 /Encoding 恢复、行内 Td/Tm 画笔跳动的空格
> 补全。char_sim 从 v0.1.2 的 21% 跃升到 **95%+**，与 PyMuPDF 对齐。
> dbnet_plus 95.5%→96.8%，arxiv_2604 70.2%→95.5%；FFFD 替换符从 99+40
> 降到 1+0。

完整对比（性能 + 精度 + 结构）见 [性能基准报告](docs/BENCHMARK.md)。

### 与 9 个主流 Python PDF 库的对比

在两个真实 arxiv 学术 PDF（14-15 页，含 LaTeX 数学公式）上，flashpdf v0.1.3
对比 pdf_oxide / PyMuPDF / pypdfium2 / pypdf / pdfminer / pdfplumber /
pdftext / pymupdf4llm / markitdown：

| 文件 | 库（按速度排序） | Mean | vs flashpdf |
|------|-----------------|-----:|------------:|
| **dbnet_plus (15p)** | **flashpdf (MT)** | **4.93ms** | — |
|                  | pypdfium2 | 47.08ms | 9.5x 慢 |
|                  | pdf_oxide | 60.22ms | 12.2x 慢 |
|                  | pypdf | 194.32ms | 39.4x 慢 |
|                  | PyMuPDF | 270.15ms | 54.8x 慢 |
|                  | pdftext / pdfminer / pdfplumber | 430-870ms | 87-176x 慢 |
|                  | pymupdf4llm | 28,219ms | 5722x 慢（OCR 回退） |
| **arxiv_2604 (14p)** | **flashpdf (MT)** | **8.44ms** | — |
|                  | pypdfium2 | 68.24ms | 8.1x 慢 |
|                  | pdf_oxide | 75.44ms | 8.9x 慢 |
|                  | PyMuPDF | 109.88ms | 13.0x 慢 |
|                  | pypdf | 362.54ms | 42.9x 慢 |
|                  | pdftext / pdfminer / pdfplumber | 480-1247ms | 57-148x 慢 |
|                  | pymupdf4llm | 23,405ms | 2773x 慢（OCR 回退） |

**flashpdf 是唯一在真实学术论文上 sub-10ms 完成文本提取的 Python 库**，比次快的
pypdfium2 快 8-10x，比 pdf_oxide 快 9-12x，比 PyMuPDF 快 13-55x。

> 注：pdf_oxide README 的 "0.8ms mean" 来自 3,830 个 1-2 页小型 PDF 语料库
> （veraPDF + Mozilla pdf.js + SafeDocs）的平均值。真实学术论文负载重 10-20 倍。
> 完整方法学、版本号、字符数、p99 见 [基准报告](docs/BENCHMARK.md)。

## 测试

```bash
# 运行全部测试
cargo test -p flashpdf-core

# 运行特定测试
cargo test -p flashpdf-core test_cmap

# 性能基准
cargo bench -p flashpdf-core
```

当前测试：**32 个核心单元测试全部通过** ✅（lib 内）+ 集成测试通过

- lib 单元测试：32 个（对象解析、xref、内容流、布局、字体、recovery）
- 流解码器 (LZW/ASCII85/RunLength/ASCIIHex)：集成覆盖

## 依赖

| Crate | 用途 |
|-------|------|
| `memchr` | SIMD 字节扫描 |
| `fast-float2` | 快速浮点解析 |
| `flate2` | zlib 解压 |
| `memmap2` | 零拷贝文件映射 |
| `rayon` | 并行迭代器 |
| `pyo3` | Python 绑定 |
| `crc32fast` | PNG CRC 校验 |
| `fnv` | 快速哈希 |
| `smallvec` | 小数组优化 |

## 路线图

- [x] 阶段 1: 自研 PDF 解析器
- [x] 阶段 2: 内容流解析 + 字体处理
- [x] 阶段 3: 布局分析
- [x] 阶段 4: 图像提取
- [x] 阶段 5: 并行化 + I/O 优化
- [x] 阶段 6: 性能基准 + PyMuPDF 对比测试
- [x] 阶段 7: PyPI 发布 + CI/CD

详见 [TODO.md](TODO.md) 获取完整的待完成事项列表。

## 许可证

MIT
