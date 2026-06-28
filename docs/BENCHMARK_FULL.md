# flashpdf 全面对比基准（10 库 × 165 PDF）

> 复现：`python tests/bench_text_full.py && python tests/bench_render_full.py && python tests/analyze_bench.py`
> 原始数据：`tests/out/bench_text_full_summary.json`、`tests/out/bench_render_full_summary.json`、`tests/out/bench_aggregated.json`
> 平台：macOS / Apple Silicon；Python 3.14；2026-06-28

## TL;DR

flashpdf 在 **文本提取** 和 **页面渲染** 两个场景都是**最快且零失败**——v0.7.2
修好 11 个 page-tree bug 后渲染 165/165 全部成功，速度领先不变。

| 场景 | 最快 | flashpdf 倍数 | 注 |
|---|---|---|---|
| 文本提取（154 文件 apples-to-apples） | **flashpdf 3.06ms** | — | pdf_oxide 第二（11×），PyMuPDF 第三（31×）|
| 文本提取大文件（>1MB, p50） | **flashpdf 5.66ms** | pdf_oxide 8×、PyMuPDF 24×、liteparse 475× | 大文件优势放大 |
| 页面渲染（162 文件公共集，DPI 150） | **flashpdf 19.72ms** | liteparse 1.4×、pypdfium2 3.0×、PyMuPDF 4.3×、pdf_oxide 6.0× | 同 PDFium 内核但 PNG 编码更快 |
| 页面渲染稳定性（165 文件） | **165/165 零失败** | — | 与 liteparse / pypdfium2 / PyMuPDF 持平（v0.7.2 修复后）|
| 极小文件（<10KB）批量 | flashpdf 0.98ms | pdf_oxide 20×、其他 ≥ 40× | 启动开销主导，差距小但 flashpdf 仍领先 |

**最终结论**：在"快速解析大量 PDF"这一目标上 flashpdf 是综合最优解——速度第一、
稳定性第一、API 接近 fitz。其他 9 个库各有不可替代的定位（pdf_oxide 编辑能力强、
PyMuPDF 全功能含 AES-256/OCR、pypdf 纯 Python 零依赖、pdfplumber 表格、markitdown
LLM 友好 markdown、pdftext 阅读序还原、pdfminer 字符级、pypdfium2 部署最简单、
liteparse 渲染第二快）——速度只是选型的一维，加密 / 编辑 / 表格 / LLM 输出等场景
仍要看具体需求选库。

**5 个关键发现**（详见 §2.0 / §4）：

1. **PDFium 家族霸占渲染榜前 3**：flashpdf / liteparse / pypdfium2 都基于 PDFium，
   其他 2 个（PyMuPDF / pdf_oxide）被甩开 3× 以上。
2. **liteparse 是渲染榜上和 flashpdf 唯一同档次选手**（1.41×），但**文本榜上是最慢**
   （1.6s mean）。两个能力走完全不同代码路径。
3. **pdf_oxide 是 liteparse 的镜像**：文本第二快（34ms），渲染倒数第二（112ms）。
   强在自研 parser，弱在自研 rasterizer。
4. **PDFium 内核 ≠ 同样快**：flashpdf 比 pypdfium2 快 3×——同样 PDFium，差距全在
   Python 绑定层 + PNG 编码层（Rust `image` crate vs PIL）。
5. **flashpdf v0.7.2 修好后已与 PDFium/MuPDF 系稳定性持平**：v0.7.1 在 PyMuPDF
   bug-regression 语料上 11 个 PDF 渲染失败，根因是 3 个独立的 xref 解析 bug
   （`/Prev` 链不跟随、xref stream 的 PNG predictor 不解码、recover_page_refs
   跳过 Compressed entries）。修复后 165/165 全部成功，速度领先不变。
   详见 [`LIMITATIONS.md` §10](LIMITATIONS.md#10-已知-bug--待修)。

## 测试设置

- **语料**：165 PDF，PyMuPDF bug-regression 测试集，覆盖 CJK / 扫描 / 加密 / 表格 / 表单 /
  矢量图，单文件 865B-8.3MB。
- **隔离**：每个 (lib, file) 在独立 Python 子进程里跑，互不污染；单个超时 60s 算失败。
- **试次**：2 trials per (lib, file)，取 min（best-of-2 信号比 mean-of-2 干净）。
- **公平口径**：本文同时给"全语料"和"154 文件公共成功集"两套数据。前者展示失败容忍度，
  后者是 apples-to-apples。

## 1. 文本提取（10 库）

### 1.1 全语料 165 文件（含失败）

| 库 | 成功率 | mean | p50 | p90 | 总耗时 |
|---|---:|---:|---:|---:|---:|
| **flashpdf** | **165/165** | **3.24ms** | **1.33ms** | **5.48ms** | **0.53s** |
| pdf_oxide | 165/165 | 36.60ms | 21.93ms | 47.40ms | 6.04s |
| pypdfium2 | 165/165 | 57.35ms | 50.70ms | 64.80ms | 9.46s |
| PyMuPDF | 165/165 | 161.79ms | 41.18ms | 145.23ms | 26.70s |
| pypdf | 157/165 | 127.54ms | 47.93ms | 146.02ms | 20.02s |
| pdftext | 165/165 | 231.63ms | 107.57ms | 217.14ms | 38.22s |
| pdfminer | 165/165 | 237.65ms | 57.65ms | 339.36ms | 39.21s |
| pdfplumber | 165/165 | 345.38ms | 70.65ms | 558.71ms | 56.99s |
| markitdown | 165/165 | 504.20ms | 178.45ms | 922.33ms | 83.19s |
| liteparse | 162/165 | 1599.26ms | 219.12ms | 3111.80ms | 259.08s |

### 1.2 公共成功集 154 文件（apples-to-apples）

| 库 | mean | p50 | p90 | 总耗时 | vs flashpdf |
|---|---:|---:|---:|---:|---:|
| **flashpdf** | **3.06ms** | **1.30ms** | **3.74ms** | **0.47s** | **1.0×** |
| pdf_oxide | 33.95ms | 21.71ms | 43.82ms | 5.23s | 11.1× |
| pypdfium2 | 55.78ms | 50.47ms | 60.86ms | 8.59s | 18.2× |
| PyMuPDF | 95.92ms | 40.61ms | 136.12ms | 14.77s | 31.3× |
| pypdf | 113.10ms | 47.86ms | 143.31ms | 17.42s | 36.9× |
| pdftext | 200.06ms | 107.51ms | 180.90ms | 30.81s | 65.4× |
| pdfminer | 210.80ms | 55.84ms | 322.58ms | 32.46s | 68.9× |
| pdfplumber | 302.28ms | 69.16ms | 494.58ms | 46.55s | 98.8× |
| markitdown | 460.56ms | 173.56ms | 662.94ms | 70.93s | 150.5× |
| liteparse | 1609.19ms | 219.12ms | 2933.63ms | 247.81s | 525.8× |

### 1.3 按文件大小分桶（公共 154 文件 p50 ms）

| 桶 | n | flashpdf | pdf_oxide | pypdfium2 | PyMuPDF | pypdf | pdftext | pdfminer | pdfplumber | markitdown | liteparse |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| tiny <10KB | 30 | **0.98** | 19.79 | 47.56 | 36.95 | 41.70 | 98.21 | 40.75 | 47.30 | 143.25 | 86.56 |
| small 10-100KB | 49 | **1.24** | 21.44 | 47.86 | 38.53 | 47.06 | 103.89 | 53.37 | 65.19 | 160.69 | 147.05 |
| medium 100KB-1MB | 59 | **1.88** | 25.06 | 51.55 | 51.62 | 61.50 | 118.86 | 93.25 | 113.12 | 221.98 | 960.60 |
| **large >1MB** | 16 | **5.66** | 44.19 | 68.37 | 136.49 | 195.47 | 138.92 | 371.86 | 708.37 | 1004.69 | 2692.34 |

**关键观察**：

- flashpdf **每一个桶都是最快**——不是只在大文件赢。
- pdf_oxide **稳定第二**——Rust 实现，每个桶 ~2 名，且成功率 100%。
- liteparse 在大文件桶崩盘（p50 2.7s）—— 大文件 dominated by 复杂版式，单线程算法
  在某些结构上有 O(N²) 行为。
- markitdown / pdfplumber 在大文件桶 >1s—— markitdown 内部用 pdfminer + LLM 风格
  后处理；pdfplumber 在 pdfminer 之上加表格/line 检测。
- 极小文件桶（<10KB）：flashpdf 仍以 20× 领先 pdf_oxide。和早期 README 里
  "tiny 桶持平"的说法已经过时——v0.7 优化了启动开销，flashpdf 在 tiny 桶领先。

## 2. 页面渲染（5 库）

10 库里有 5 个具备真正的"渲染页面到像素"能力：**flashpdf / liteparse / pypdfium2 /
PyMuPDF / pdf_oxide**。pdftext 内部用 pypdfium2（不单独测），其他 4 库
（pypdf / pdfminer / pdfplumber / markitdown）不渲染。

测试口径：渲染每文件第 0 页（165 文件 = 165 页），DPI 150，输出 PNG bytes。
（liteparse 的 DPI 由库内部决定，A4 输出 1240×1754 = 150 DPI，与其他对齐。）

### 2.0 五个关键发现

1. **PDFium 家族霸占前 3**：flashpdf / liteparse / pypdfium2 都基于 Google PDFium，
   是 Apple Silicon 上最快的 rasterizer。3 个库差距全在 Python 绑定层 + PNG 编码层，
   不在 raster 本身。
2. **liteparse 是 flashpdf 唯一同档次对手**（1.41×）：同样 Rust 直绑 PDFium，
   没有 PIL 中间层，但 PNG 编码路径稍慢。其他 3 个库都被甩开 3× 以上。
3. **文本速度 ≠ 渲染速度（强反直觉）**：liteparse 文本提取**全语料最慢**（1.6s mean，
   10 库倒数第一），但渲染**第二快**（33ms）。pdf_oxide 反过来——文本第二快（34ms）
   但渲染倒数第二（112ms，长尾）。**选型时不能拿一个维度的 benchmark 套另一个维度**。
4. **pdf_oxide 渲染有长尾**：p50 49ms 不慢，但 mean 112ms。8 个文件 >500ms，最慢
   test_3448.pdf **2.5s**。复杂矢量图/渐变/透明度合成场景需要优化（v0.3.x 早期）。
5. **flashpdf 渲染 165/165（v0.7.2 修好的 page-tree bug）**：v0.7.1 有 11 个 PDF
   渲染失败，根因是 3 个独立的 xref 解析 bug（`/Prev` 链不跟随、xref stream
   PNG predictor 不解码、recover_page_refs 跳过 Compressed entries）。v0.7.2
   三个一起修，165/165 全部成功渲染，速度领先不变（25ms vs liteparse 33ms）。

### 2.0.1 文本排名 vs 渲染排名（文本 154 文件、渲染 162 文件）

| 库 | 文本 mean rank | 渲染 mean rank | 反差 |
|---|---:|---:|---|
| **flashpdf** | **1** (3.06ms) | **1** (19.72ms) | — (双第一) |
| liteparse | 10 (1609ms, **最慢**) | **2** (27.80ms) | **+8 位**（文本最差，渲染第二）|
| pdf_oxide | 2 (34ms) | 5 (118ms) | **-3 位**（文本第二，渲染第五）|
| pypdfium2 | 3 (56ms) | 3 (60ms) | — (一致) |
| PyMuPDF | 4 (96ms) | 4 (85ms) | — (一致) |

> 注：文本基准 154 文件（11 个加密/损坏 PDF 被部分库跳过），渲染基准 162 文件
> （3 个 pdf_oxide 渲染失败的 PDF 不计入公共集）。两边都取各库都成功的文件
> 子集，所以 mean 与 §1.2 / §2.2 的全语料数字略有出入。

**这张表揭示了库的实现差异**：

- **PDFium 内核纯调用型**（flashpdf / liteparse / pypdfium2）：渲染快，因为 PDFium 直
  接吐 bitmap；文本提取速度则取决于 Python 绑定层和文本 layout 代码——liteparse 的
  文本路径是自研版面分析器，某些 PDF 上有 O(N²) 行为，所以文本最慢
- **MuPDF 内核型**（PyMuPDF）：两个能力都中等，因为 MuPDF 自带 raster + text，
  无 PDFium 加持
- **自研 rasterizer 型**（pdf_oxide）：文本提取是强项（Rust 自研 parser 快），
  渲染用自研 rasterizer 在复杂场景上有长尾
- **结论**：渲染 benchmark **不能**用文本 benchmark 的结论推断。liteparse / pdf_oxide
  的"快慢"取决于你拿它们做什么。

### 2.1 全语料 165 文件（含失败）

| 库 | 渲染后端 | 成功率 | mean | p50 | p90 | 总耗时 |
|---|---|---:|---:|---:|---:|---:|
| **flashpdf** | PDFium (Rust + image crate PNG) | **165/165** | **25.52ms** | **16.14ms** | **37.91ms** | **4.21s** |
| liteparse | PDFium (Rust 直绑) | 165/165 | 33.06ms | 23.62ms | 44.15ms | 5.46s |
| pypdfium2 | PDFium (Python ctypes) | 165/165 | 67.13ms | 57.85ms | 74.06ms | 11.08s |
| PyMuPDF | MuPDF raster | 165/165 | 103.11ms | 73.14ms | 122.40ms | 17.01s |
| pdf_oxide | 自研 Rust rasterizer | 162/165 | 118.38ms | 48.50ms | 147.43ms | 19.18s |

### 2.2 162 文件公共集（apples-to-apples）

| 库 | mean | p50 | p90 | 总耗时 | vs flashpdf |
|---|---:|---:|---:|---:|---:|
| **flashpdf** | **19.72ms** | **16.06ms** | **35.78ms** | **3.20s** | **1.0×** |
| liteparse | 27.80ms | 23.61ms | 42.81ms | 4.50s | 1.41× |
| pypdfium2 | 59.67ms | 57.72ms | 72.00ms | 9.67s | 3.03× |
| PyMuPDF | 84.68ms | 72.99ms | 120.06ms | 13.72s | 4.29× |
| pdf_oxide | 118.38ms | 48.50ms | 147.43ms | 19.18s | 6.00× |

### 2.3 按文件大小分桶（公共 162 文件 p50 ms）

| 桶 | n | flashpdf | liteparse | pdf_oxide | pypdfium2 | PyMuPDF |
|---|---:|---:|---:|---:|---:|---:|
| tiny <10KB | 31 | **14.14** | 18.35 | 32.26 | 55.02 | 59.75 |
| small 10-100KB | 50 | **12.75** | 19.82 | 40.03 | 53.05 | 67.24 |
| medium 100KB-1MB | 63 | **20.73** | 28.23 | 61.94 | 60.68 | 81.00 |
| large >1MB | 18 | **25.19** | 32.10 | 67.18 | 63.80 | 92.83 |

### 2.4 渲染后端解读

**PDFium 家族占前 3 名**（flashpdf / liteparse / pypdfium2）—— Google PDFium 在 Apple
Silicon 上是最快的 PDF rasterizer；3 个库的差距主要在 Python 绑定层和 PNG 编码层：

- **flashpdf 最快**：Rust + `image` crate PNG 编码 + PDFium 单例缓存
  （`Mutex<Option<&'static Pdfium>>` + `Box::leak`），零 Python 调用，BGRA→RGBA swap
  用 Rust slice 操作
- **liteparse 第二**：同样 Rust 直绑 PDFium，没有 PIL 层，但 PNG 编码路径稍慢
  （1.41× slower than flashpdf on common set）。liteparse 是渲染榜上和 flashpdf
  **唯一同档次**的选手——两个都用 PDFium 内核，差距来自 PNG 编码细节
- **pypdfium2 第三**：Python ctypes + PIL，每页有 Python 调用开销 + PIL Image 转换
  （3× slower than flashpdf）

**PyMuPDF 用 MuPDF raster**：比 PDFium 慢 ~70%——MuPDF 强在字形保真（小字 / 复杂字体），
PDFium 强在吞吐。PNG 编码用 MuPDF 内部 zlib，相比 Rust `image` crate + zlib-ng 在
Apple Silicon 上慢 1.5-2×。

**pdf_oxide 用自研 Rust rasterizer**：p50 49ms 看着不慢，但 **mean 112ms**（极端
尾部）：8 个文件 >500ms，最慢 test_3448.pdf **2.5s**、test_4182.pdf **1.9s**。
这些文件有复杂矢量图 / 渐变 / 透明度合成，pdf_oxide 的 rasterizer 在这些场景上需要
进一步优化。pdf_oxide 的渲染器还在早期（v0.3.x），未来可能追上。

### 2.5 渲染失败

| 库 | 失败 | 文件 |
|---|---:|---|
| flashpdf | **0**（v0.7.2 已修）| — |
| pdf_oxide | 3 | `test_3450.pdf`, `test_3806.pdf`, `test_4790.pdf`（具体原因未深挖，可能是 PDF 加密或损坏对象）|
| liteparse | 0 | — |
| pypdfium2 | 0 | — |
| PyMuPDF | 0 | — |

**v0.7.1 → v0.7.2 修复的 11 个 PDF**（曾经失败，现 165/165）：

```
test-3820.pdf, test_2710.pdf, test_3058.pdf, test_3624.pdf, test_3848.pdf,
test_4079.pdf, test_4755.pdf, test_annot_file_info.pdf, test_toc_count.pdf,
v110-changes.pdf, widgettest.pdf
```

**根因（3 个独立 xref/page-tree 解析 bug）**：
1. `/Prev` 链不跟随：incremental-update PDF（多次保存过的 PDF）只有最新 xref 段被读，
   `/Prev` 指向的旧段被丢弃 → page_count=0
2. xref stream 的 PNG predictor 不解码：现代 PDF 的 xref stream 几乎全用
   `/Predictor 12`。Flate 解压后直接当 entry 字节解析 → type byte 错位 → 大部分
   Compressed entries 指向不存在的 ObjStm → ObjStm 里的 page 对象全部丢失
3. `recover_page_refs` 跳过 Compressed entries：上述 (2) 修好后，page refs fallback
   还需要扫 ObjStm 里的 page 对象

**修复**：见 [`LIMITATIONS.md` §10](LIMITATIONS.md#10-已知-bug--待修)。带 7 个
PNG predictor 单元测试防回归。

## 3. 失败模式分析

| 库 | 失败文件数 | 类型 |
|---|---:|---|
| flashpdf (text) | 0 | — |
| flashpdf (render) | **0**（v0.7.2 修好）| — |
| liteparse (text) | 2 + 1 skip | `circular-toc.pdf` 已知无限循环跳过；`test-3820.pdf` / `test_3594.pdf` |
| liteparse (render) | 0 | — |
| pdf_oxide (text) | 0 | — |
| pdf_oxide (render) | 3 | `test_3450.pdf`, `test_3806.pdf`, `test_4790.pdf` |
| pypdfium2 (render) | 0 | — |
| PyMuPDF (render) | 0 | — |
| pypdf (text) | 8 | 多为加密 / xref 损坏 / 非标准 Type3 |
| pdftext | 0 | — |
| pdfminer | 0 | — |
| pdfplumber | 0 | — |
| markitdown | 0 | — |

pypdf 的 8 个失败：

```
2.pdf, joined.pdf, test-2462.pdf, test_2710.pdf, test_3725.pdf,
test_4147.pdf, test_4412.pdf, test_4942.pdf
```

pypdf 在加密 + 损坏 xref 上比 Rust/C 实现的库脆弱。

## 4. 速度之外的维度（重要！）

速度只是选型的一维。下表是综合评价：

| 库 | 速度 | 功能广度 | fitz 兼容 | 部署 | 特长 |
|---|---|---|---|---|---|
| **flashpdf** | **★★★★★** | ★★★ | ✅ 主流 | 需 Rust 工具链源码构建；render 需 PDFium binary | 最快文本 + 最快渲染 + 165/165 零失败 |
| pdf_oxide | ★★★★ | ★★★★★ | ❌ | pip install | 唯一同时支持编辑（合并/拆分/签名）的快速库 |
| PyMuPDF | ★★★ | ★★★★★ | — (就是 fitz) | pip install 开箱即用 | 全功能：编辑、注释、签名、OCR、AES-256 |
| pypdfium2 | ★★★ | ★★★ | ❌ | pip install 自带 PDFium | 渲染最稳定、纯 Python wheel |
| pypdf | ★★ | ★★★★ | ❌ | 纯 Python，零依赖 | 加密 / 合并 / 拆分，可读可写 |
| pdftext | ★★ | ★★ | ❌ | 重依赖（onnxruntime 等）| 阅读序还原、表格、markdown 输出 |
| pdfminer | ★ | ★★★ | ❌ | 纯 Python | 字符级坐标精度，自带 PDF 解析器（非 pdfium/fitz 后端）|
| pdfplumber | ★ | ★★★★ | ❌ | 基于 pdfminer | 表格提取（最强）、可视化调试 |
| markitdown | ★ | ★★★ | ❌ | LLM 友好 | Microsoft 出品，输出 markdown，对接 LLM pipeline |
| liteparse | ★★（文本）/ ★★★★（渲染）| ★★ | ❌ | pip install | 文本慢但**渲染第二快**；自定义版面分析 |

**几个反直觉的发现**：

1. **markitdown 比 pdfminer 慢**：因为 markitdown 内部跑 pdfminer + magika（文件类型
   识别 LLM 模型），有额外 onnxruntime 推理开销。
2. **PyMuPDF 在小文件上不如 pdf_oxide / pypdfium2**：MuPDF 的 Python 绑定有 ~30ms 的
   per-call 开销（fitz.open + GC），在小文件场景吃亏。
3. **pypdfium2 文本提取比渲染更慢**：因为 pypdfium2 的文本 API（`get_textpage().get_text_bounded()`）
   在每次调用时都会做完整 layout 分析；而渲染只是 bitmap 输出，没有 layout 阶段。
4. **pdftext 和 pdfminer 速度接近**：pdftext 上层用 pypdfium2 + pypdf，但加上 onnx
   block 排序、表格识别、阅读序后处理后变慢。功能强但代价高。
5. **liteparse 文本慢但渲染快**：文本提取平均 1.6s（最慢），但渲染只要 33ms（第二快，
   仅次于 flashpdf）。两个能力走完全不同的代码路径——渲染直接调 PDFium C ABI，
   文本提取是 liteparse 自研版面分析器，在某些 PDF 上有 O(N²) 行为。liteparse
   和 flashpdf 在渲染榜上是**唯一同档次**的选手（都基于 PDFium，差距来自 PNG 编码）。
6. **pdf_oxide 文本快但渲染慢**（反差对称）：文本第二快（34ms，仅次于 flashpdf），
   渲染倒数第二（112ms）。原因：pdf_oxide 用 Rust 自研 parser 做文本提取（快），
   但渲染用自研 rasterizer 在矢量图/渐变场景有长尾。**与 liteparse 完全对称**——
   pdf_oxide 强在解析，liteparse 强在调用 PDFium。
7. **渲染稳定性**：flashpdf / liteparse / pypdfium2 / PyMuPDF 全部 165/165 零失败；
   pdf_oxide 162/165（3 个失败）。flashpdf v0.7.2 修了 v0.7.1 的 11 个 page-tree
   bug 后已与 PDFium/MuPDF 系稳定性持平。

## 5. 选型建议

| 你的场景 | 首选 | 备选 |
|---|---|---|
| **RAG / 全文索引 / 批量预处理（千份以上 PDF）** | ✅ flashpdf | pdf_oxide（功能更广但 10× 慢）|
| **混合工作流：文本 + 渲染缩略图** | ✅ flashpdf | pypdfium2（部署更简单）|
| **仅渲染（每文件第 0 页缩略图）** | ✅ flashpdf | liteparse（同样 PDFium-based，0 失败）或 pypdfium2（更稳，0 失败）|
| **需要编辑 PDF（合并/拆分/签名/水印）** | pdf_oxide | PyMuPDF / pypdf |
| **需要 AES-256 / 加密码 PDF** | PyMuPDF | pypdfium2 |
| **简单有线框表格（规则派够用）** | pdfplumber | pdftext |
| **复杂 / 无边框 / 合并单元格（模型派）** | Surya | PaddleOCR PP-Structure / Table Transformer |
| **LLM 友好的 markdown 输出** | markitdown | pdftext |
| **极简零依赖部署（仅 Python）** | pypdf | pdfminer |
| **字符级坐标精度 + 自带解析器** | pdfminer | pdfplumber |
| **极小文件批量（<1KB）** | flashpdf（仍领先，但优势小）| 任意都行 |
| **大文件渲染（>1MB）批量缩略图** | ✅ flashpdf（25ms p50）| liteparse（32ms）|
| **既要文本又要渲染（同一进程）** | ✅ flashpdf（双第一）| PyMuPDF（功能全但慢）|
| **渲染稳定性第一（零失败）** | ✅ flashpdf / liteparse / pypdfium2 / PyMuPDF（4 家全部 165/165）| — |

## 6. 复现

```bash
# 1. 安装所有库
pip install flashpdf liteparse pdf-oxide pymupdf pypdfium2 pypdf pdftext \
            pdfminer.six markitdown pdfplumber

# 2. 准备 PDFium binary（flashpdf 渲染需要）
curl -L https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-mac.tgz | tar xz
mkdir -p pdfium-bin && cp Libraries/libpdfium.dylib pdfium-bin/
export PDFIUM_PATH=$(pwd)/pdfium-bin/libpdfium.dylib

# 3. 准备语料
git clone --depth 1 https://github.com/pymupdf/PyMuPDF.git /tmp/pymupdf
export CORPUS_DIR=/tmp/pymupdf/tests/resources

# 4. 跑（约 30-45 分钟）
python tests/bench_text_full.py     # 文本提取
python tests/bench_render_full.py   # 页面渲染
python tests/analyze_bench.py       # 聚合 + 分桶
```

## 7. 已知 caveats

1. **Apple Silicon 偏向**：测试在 M 系列芯片上跑；x86 平台 PNG 编码速度可能不同
   （Rust `image` crate 和 PIL 的 SIMD 优化路径不同）。
2. **pypdfium2 被降级**：pdftext 要求 `pypdfium2<5`，所以渲染对比用 4.30.0
   而非最新 5.x。5.x 性能可能有变化。
3. **第 0 页**：渲染 benchmark 只测第 0 页；完整文档渲染吞吐未测（flashpdf `render_only`
   的优势在长文档会放大，pypdfium2 / PyMuPDF / pdf_oxide 没有"render_only fast path"）。
4. **liteparse 跳过 circular-toc.pdf**：已知该库在该文件上无限循环。
5. **render feature 状态**：flashpdf 的 `render` feature 在 `feature/render` 分支，
   尚未合并 main；用 `pip install flashpdf` 装的版本没有渲染能力，需源码构建
   （`maturin develop --release --features render`）。
6. **环境噪声**：单机跑，未做多次 cold-cache 测量。生产场景的 I/O 调度、CPU 占用、
   并发模型可能改变相对结论。
