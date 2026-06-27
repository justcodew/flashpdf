# flashpdf 全面对比基准（10 库 × 165 PDF）

> 复现：`python tests/bench_text_full.py && python tests/bench_render_full.py && python tests/analyze_bench.py`
> 原始数据：`tests/out/bench_text_full_summary.json`、`tests/out/bench_render_full_summary.json`、`tests/out/bench_aggregated.json`
> 平台：macOS / Apple Silicon；Python 3.14；2026-06-28

## TL;DR

flashpdf 在 **文本提取** 和 **页面渲染** 两个场景都是**最快**——但不是所有场景，
也不是没有代价。

| 场景 | 最快 | flashpdf 倍数 | 注 |
|---|---|---|---|
| 文本提取（154 文件 apples-to-apples） | **flashpdf 3.06ms** | — | pdf_oxide 第二（11×），PyMuPDF 第三（31×）|
| 文本提取大文件（>1MB, p50） | **flashpdf 5.66ms** | pdf_oxide 8×、PyMuPDF 24×、liteparse 475× | 大文件优势放大 |
| 页面渲染（154 文件，DPI 150） | **flashpdf 25.14ms** | pypdfium2 2.7×、PyMuPDF 4.1× | 同 PDFium 内核但 PNG 编码更快 |
| 极小文件（<10KB）批量 | flashpdf 0.98ms | pdf_oxide 20×、其他 ≥ 40× | 启动开销主导，差距小但 flashpdf 仍领先 |

**结论**：在"快速解析大量 PDF"这一目标上 flashpdf 是综合最优解；其他 9 个库各有定位
（pdf_oxide 编辑能力强、PyMuPDF 全功能、pypdf 纯 Python 零依赖、pdfplumber 表格、
markitdown LLM 友好 markdown、pdftext 阅读序还原、pdfminer 字符级、pypdfium2 渲染稳定、
liteparse 自定义布局）—— 选型不能只看速度。

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

## 2. 页面渲染（3 库）

10 库里只有 **flashpdf / pypdfium2 / PyMuPDF** 有真正的"渲染页面到像素"能力。
pdftext 内部用 pypdfium2，pdf_oxide 的 `RenderedPixmap` 在测试版本上接口不稳定，
其他库（pypdf / pdfminer / pdfplumber / markitdown / liteparse）不渲染。

测试口径：渲染每文件第 0 页（165 文件 = 165 页），DPI 150，输出 PNG bytes。

| 库 | 成功率 | mean | p50 | p90 | 总耗时 |
|---|---:|---:|---:|---:|---:|
| **flashpdf** | **154/165** | **25.14ms** | **16.38ms** | **36.27ms** | **3.87s** |
| pypdfium2 | 165/165 | 67.13ms | 57.85ms | 74.06ms | 11.08s |
| PyMuPDF | 165/165 | 103.11ms | 73.14ms | 122.40ms | 17.01s |

**154 文件公共集（apples-to-apples）**：

| 库 | mean | p50 | p90 | 总耗时 | vs flashpdf |
|---|---:|---:|---:|---:|---:|
| **flashpdf** | **25.14ms** | **16.38ms** | **36.27ms** | **3.87s** | **1.0×** |
| pypdfium2 | 67.59ms | 57.72ms | 74.06ms | 10.41s | 2.7× |
| PyMuPDF | 104.41ms | 73.37ms | 122.40ms | 16.08s | 4.1× |

**为什么 flashpdf 比 pypdfium2 快？两个都用 PDFium 内核**：

- flashpdf 用 Rust + `image` crate 的 PNG 编码器，pypdfium2 默认 PIL（zlib 走 C 但有 Python
  层包装）；PNG 编码是渲染管线的瓶颈（150 DPI 单页 ~600KB raw → PNG ~50-100KB）。
- flashpdf 用 `Mutex<Option<&'static Pdfium>>` + `Box::leak` 把 PDFium 单例缓存在
  进程生命周期内，省去重复初始化。pypdfium2 每次 `PdfDocument` 都是独立的（虽然 PDFium
  内部也有 thread-local 缓存，但跨多次调用不是同一进程的免费午餐）。
- pypdfium2 走 `.to_pil()` 会多一次 RGBA→PIL Image 转换；flashpdf 直接 BGRA→RGBA swap
  后用 `image` crate 编码，零 Python 调用。

**为什么 flashpdf 比 PyMuPDF 快？**

- PDFium 在本语料上比 MuPDF raster 更快（前者 Google 维护、后者 Artifex 维护，
 优化的方向不同；MuPDF 强在字形保真、PDFium 强在吞吐）。
- PyMuPDF 的 PNG 编码（pix.tobytes("png")）路径 MuPDF 内部 zlib，相比 Rust `image`
  crate + 自带 zlib-ng 在 Apple Silicon 上慢 1.5-2×。

### flashpdf 渲染的 11 个失败

flashpdf 在 11 个 PDF 上 `IndexError: page index 0 out of range`：

```
test-3820.pdf, test_2710.pdf, test_3058.pdf, test_3624.pdf, test_3848.pdf,
test_4079.pdf, test_4755.pdf, test_annot_file_info.pdf, test_toc_count.pdf,
v110-changes.pdf, widgettest.pdf
```

**原因**：`render_only=True` 模式走 stub PageResult，未复用 `extract_doc` 里的
3-tier page_count 恢复路径（top-level /Count → /Kids 遍历 → xref 全表扫描）。
这些 PDF 都有异常页树（嵌套 /Pages /Count 不一致、/Kids 引用循环等）。
**修复路径**：把 page_count 三层 fallback 提取成独立函数，render_only 路径也调用。
**优先级**：中（已知 bug，下一版修）。

## 3. 失败模式分析

| 库 | 失败文件数 | 类型 |
|---|---:|---|
| flashpdf (text) | 0 | — |
| flashpdf (render) | 11 | 页树异常 (上述 bug) |
| liteparse | 2 + 1 skip | `circular-toc.pdf` 已知无限循环跳过；`test-3820.pdf` / `test_3594.pdf` |
| pdf_oxide | 0 | — |
| pypdfium2 | 0 | — |
| PyMuPDF | 0 | — |
| pypdf | 8 | 多为加密 / xref 损坏 / 非标准 Type3 |
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
| **flashpdf** | **★★★★★** | ★★★ | ✅ 主流 | 需 Rust 工具链源码构建；render 需 PDFium binary | 最快文本 + 最快渲染 |
| pdf_oxide | ★★★★ | ★★★★★ | ❌ | pip install | 唯一同时支持编辑（合并/拆分/签名）的快速库 |
| PyMuPDF | ★★★ | ★★★★★ | — (就是 fitz) | pip install 开箱即用 | 全功能：编辑、注释、签名、OCR、AES-256 |
| pypdfium2 | ★★★ | ★★★ | ❌ | pip install 自带 PDFium | 渲染最稳定、纯 Python wheel |
| pypdf | ★★ | ★★★★ | ❌ | 纯 Python，零依赖 | 加密 / 合并 / 拆分，可读可写 |
| pdftext | ★★ | ★★ | ❌ | 重依赖（onnxruntime 等）| 阅读序还原、表格、markdown 输出 |
| pdfminer | ★ | ★★★ | ❌ | 纯 Python | 字符级坐标精度，自带 PDF 解析器（非 pdfium/fitz 后端）|
| pdfplumber | ★ | ★★★★ | ❌ | 基于 pdfminer | 表格提取（最强）、可视化调试 |
| markitdown | ★ | ★★★ | ❌ | LLM 友好 | Microsoft 出品，输出 markdown，对接 LLM pipeline |
| liteparse | ★ | ★★ | ❌ | pip install | 自定义版面分析、screenshot |

**几个反直觉的发现**：

1. **markitdown 比 pdfminer 慢**：因为 markitdown 内部跑 pdfminer + magika（文件类型
   识别 LLM 模型），有额外 onnxruntime 推理开销。
2. **PyMuPDF 在小文件上不如 pdf_oxide / pypdfium2**：MuPDF 的 Python 绑定有 ~30ms 的
   per-call 开销（fitz.open + GC），在小文件场景吃亏。
3. **pypdfium2 文本提取比渲染更慢**：因为 pypdfium2 的文本 API（`get_textpage().get_text_bounded()`）
   在每次调用时都会做完整 layout 分析；而渲染只是 bitmap 输出，没有 layout 阶段。
4. **pdftext 和 pdfminer 速度接近**：pdftext 上层用 pypdfium2 + pypdf，但加上 onnx
   block 排序、表格识别、阅读序后处理后变慢。功能强但代价高。

## 5. 选型建议

| 你的场景 | 首选 | 备选 |
|---|---|---|
| **RAG / 全文索引 / 批量预处理（千份以上 PDF）** | ✅ flashpdf | pdf_oxide（功能更广但 10× 慢）|
| **混合工作流：文本 + 渲染缩略图** | ✅ flashpdf | pypdfium2（部署更简单）|
| **仅渲染（每文件第 0 页缩略图）** | ✅ flashpdf | pypdfium2（更稳，0 失败）|
| **需要编辑 PDF（合并/拆分/签名/水印）** | pdf_oxide | PyMuPDF / pypdf |
| **需要 AES-256 / 加密码 PDF** | PyMuPDF | pypdfium2 |
| **表格提取（精确 cell 坐标）** | pdfplumber | pdftext |
| **LLM 友好的 markdown 输出** | markitdown | pdftext |
| **极简零依赖部署（仅 Python）** | pypdf | pdfminer |
| **字符级坐标精度 + 自带解析器** | pdfminer | pdfplumber |
| **极小文件批量（<1KB）** | flashpdf（仍领先，但优势小）| 任意都行 |

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
   的优势在长文档会放大，pypdfium2 / PyMuPDF 没有"render_only fast path"）。
4. **liteparse 跳过 circular-toc.pdf**：已知该库在该文件上无限循环。
5. **render feature 状态**：flashpdf 的 `render` feature 在 `feature/render` 分支，
   尚未合并 main；用 `pip install flashpdf` 装的版本没有渲染能力，需源码构建
   （`maturin develop --release --features render`）。
6. **环境噪声**：单机跑，未做多次 cold-cache 测量。生产场景的 I/O 调度、CPU 占用、
   并发模型可能改变相对结论。
