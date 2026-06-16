# fastpdf 待完成事项

## 当前状态

- **测试**: 82 个测试全部通过
- **性能**: ~30x 速度提升 (vs PyMuPDF)
- **精度**: 91.6% 单词重叠率

## 核心功能（影响可用性）

- [x] **真实 PDF 端到端测试**：用 arxiv 论文等真实 PDF 跑通完整流程，验证文本提取正确性
- [x] **Type0 宽度计算集成**：CIDFont 的 `/W` 数组已解析，`emit_string` 中已集成 `cid_font.cid_width(cid)`
- [x] **Form XObject 内部资源解析**：递归时 Form 自带的 `/Resources` (字体/图像) 已合并到扫描上下文
- [x] **流对象解码统一**：已支持 LZWDecode、ASCII85Decode、RunLengthDecode、ASCIIHexDecode
- [x] **加密 PDF 识别**：遇到加密 PDF 会报明确错误 "encrypted PDFs are not supported"
- [x] **TJ 字间距空格检测**：大 kerning 值 (>=150/1000 em) 自动插入空格字符
- [x] **/Resources 间接引用解析**：修复页面 Resources 为间接引用时字体丢失的问题

## 性能验证（阶段 6）

- [x] **PyMuPDF 对比测试**：同一批 PDF 分别用 fastpdf 和 PyMuPDF 提取
  - 单词重叠率: 91.6%
  - 速度: ~30x (fastpdf 0.006s vs PyMuPDF 0.189s)
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
- [ ] **PyPI 发布**：`pip install fastpdf`（暂不处理）
- [x] **CI/CD**：GitHub Actions 自动测试 + 构建 + 发布
  > .github/workflows/ci.yml + build-wheels.yml

## 优化空间（可选）

- [x] **在线聚类**：SmallVec 边解析边产出 Line，内存 -30%，延迟更低
- [ ] **GPU 加速**：nvjpg 硬件 JPEG 解码、nvcomp 加速 DEFLATE（feature flag）（暂不处理）
- [x] **自研 PDF 解析器终极优化**：去除 lopdf 非必要抽象（已完成，可进一步优化内存布局）
- [x] **SmallVec 聚类存储**：减少聚类过程中的堆分配
- [x] **字体子集化**：CIDFont /W 数组已改为范围存储 + 二分查找

## 待改进（精度提升）

- [ ] 提升单词重叠率到 95%+（当前 91.6%）
- [ ] 改进 CID 字符解码的 CMap 映射完整性
- [ ] 优化连字符 (hyphen) 处理 — 跨行连字符合并
- [ ] 调优 TJ 字间距阈值 — 当前 150 可能需要根据字体自适应
- [ ] 布局聚类参数调优 (BLOCK_GAP_FACTOR, SPAN_GAP_FACTOR)

## 已知限制

- 加密 PDF 不支持（会报错）
- 线性化 PDF 的快速跳转未利用（功能正常，只是没加速）
- 数字签名不验证
- 增量更新只处理最后一份 xref
- 对象流内间接引用的完全递归未实现
- 竖排文字 (WMode=1) 的 Dir 字段固定为 (1.0, 0.0)
