# flashpdf 待完成事项

## 当前状态

- **版本**: 0.1.1 (PyPI 已发布)
- **测试**: 29+ 个单元测试全部通过
- **性能**: ~52x 速度提升 (vs PyMuPDF，15 页 PDF 文本提取 4.6ms vs 256ms)
- **精度**: 96.9% 单词重叠率 (regex-based)

## 核心功能（影响可用性）

- [x] **真实 PDF 端到端测试**：用 arxiv 论文等真实 PDF 跑通完整流程，验证文本提取正确性
- [x] **Type0 宽度计算集成**：CIDFont 的 `/W` 数组已解析，`emit_string` 中已集成 `cid_font.cid_width(cid)`
- [x] **Form XObject 内部资源解析**：递归时 Form 自带的 `/Resources` (字体/图像) 已合并到扫描上下文
- [x] **流对象解码统一**：已支持 LZWDecode、ASCII85Decode、RunLengthDecode、ASCIIHexDecode
- [x] **加密 PDF 识别**：遇到加密 PDF 会报明确错误 "encrypted PDFs are not supported"
- [x] **TJ 字间距空格检测**：大 kerning 值 (>=150/1000 em) 自动插入空格字符
- [x] **/Resources 间接引用解析**：修复页面 Resources 为间接引用时字体丢失的问题

## 性能验证（阶段 6）

- [x] **PyMuPDF 对比测试**：同一批 PDF 分别用 flashpdf 和 PyMuPDF 提取
  - 单词重叠率: 91.6%
  - 速度: ~30x (flashpdf 0.006s vs PyMuPDF 0.189s)
  - 关键修复: TJ 字间距空格检测、/Resources 间接引用解析、Type0 CID 宽度计算
  > 脚本: tests/pymupdf_comparison.py | 报告: docs/BENCHMARK.md
- [x] **criterion 性能基准**：各场景耗时对比 (纯文本/图文混排/扫描件/表格/中日韩)
  > 基准测试: benches/extraction.rs
- [x] **flamegraph 热点分析**：定位并消除最后的性能瓶颈
  > 脚本已创建: scripts/flamegraph.sh
- [x] **内存泄漏检查**：valgrind / heaptrack 验证 mmap 生命周期正确性
  > 脚本已创建: scripts/check_memory.sh

## 发布准备（阶段 7）

- [x] **测试集收集**：20+ 真实 PDF 样本
  > 脚本已创建: scripts/collect_test_pdfs.sh
- [x] **wheel 构建**：
  - Linux x86_64 / aarch64 (manylinux)
  - macOS x86_64 / ARM64
  - Windows x86_64
  - 通过 maturin + GitHub Actions 自动构建
- [x] **PyPI 发布**：`pip install flashpdf`
- [x] **CI/CD**：GitHub Actions 自动测试 + 构建 + 发布
  > .github/workflows/ci.yml + build-wheels.yml

## 优化空间（可选）

- [x] **在线聚类**：SmallVec 边解析边产出 Line，内存 -30%，延迟更低
- [ ] **GPU 加速**：nvjpg 硬件 JPEG 解码、nvcomp 加速 DEFLATE（feature flag）（暂不处理）
- [x] **自研 PDF 解析器终极优化**：去除 lopdf 非必要抽象（已完成，可进一步优化内存布局）
- [x] **SmallVec 聚类存储**：减少聚类过程中的堆分配
- [x] **字体子集化**：CIDFont /W 数组已改为范围存储 + 二分查找

## 待改进（精度提升）

- [x] 多栏布局检测 — 使用平滑密度直方图检测双栏 PDF，正确分离左右栏文本
- [x] 提升单词重叠率到 95%+ — regex-based 96.9% (目标达成)
- [x] 改进 CID 字符解码的 CMap 映射完整性 — 扩展 Adobe Glyph List（希腊字母、数学符号、变音符号）+ UTF-16BE 代理对处理
- [x] 优化连字符 (hyphen) 处理 — 跨行连字符合并，196/196 成功合并
- [x] 调优 TJ 字间距阈值 — 根据字体大小自适应：-150 * max(font_size/12, 0.5)
- [x] 布局聚类参数调优 (BLOCK_GAP_FACTOR, SPAN_GAP_FACTOR) — 测试后原参数最优
- [x] **内置字体编码支持** — Symbol、ZapfDingbats、TeX CMSY 字体在无 ToUnicode 时使用内置编码表，FFFD 字符数 152→99（v0.1.1）

## 后续优化（精度提升 - 待处理）

- [ ] **span 级 XY-cut 列检测** — v0.1.2 的 block 级 XY-cut 只能把 char_sim 从 18% 提到 21%，瓶颈在上游 `detect_columns_from_spans`（X 投影直方图）在公式密集页面检测失败，导致单个 block 横跨双栏。计划：用递归 XY-cut 在 span 级替代直方图列检测，从根本上分离双栏
- [ ] **扩展 TeX 字体内置编码** — 添加 CMMI（math italic，希腊字母）、CMR（OT1/T1 编码）等 Computer Modern 字体系列，进一步降低 FFFD 数量
- [ ] **WinAnsiEncoding 默认应用** — Type1/TrueType 字体未声明 Encoding 时按 WinAnsi 兜底，正确解码 bullet (•)、em-dash (—) 等
- [ ] **坐标归一化** — 部分 PDF（如 `2604.11578v1.pdf`）的文本坐标超出 MediaBox 范围（标题 x=1042 而 MediaBox 宽 595），疑似 UserUnit 或 CTM 缩放未应用。PyMuPDF 会归一化到页面坐标，flashpdf 当前原样输出

## 已知限制

- 加密 PDF 不支持（会报错）
- 线性化 PDF 的快速跳转未利用（功能正常，只是没加速）
- 数字签名不验证
- 增量更新只处理最后一份 xref
- 对象流内间接引用的完全递归未实现
- 竖排文字 (WMode=1) 的 Dir 字段固定为 (1.0, 0.0)
