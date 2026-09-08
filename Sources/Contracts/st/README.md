# Aurora ST contracts

本目录保存 Aurora ST 语言、诊断和逻辑地址映射的规范源。编译器、参考执行器、AOT 和 Studio 必须消费同一版本，不得各自复制或扩展语义。

## Preview v1

- [Aurora ST Language Preview 1.0](v1/language.md)：词法、语法、类型、POU、执行顺序、标准函数、算术/Fault 与诊断目录。
- [Aurora ST normative EBNF](v1/aurora-st.ebnf)：Preview 1.0 的规范上下文无关文法。
- [Aurora ST Address Mapping Preview 1.0](v1/address-mapping.md)：`%I/%Q/%M`、TagId、payload-local handle、Device Mapping 和重叠/多写者规则。

Preview reader/compiler 只接受其明确声明支持的精确 minor。未知 major/minor 必须拒绝，不得猜测、降级或按其他 ST 方言解释。
