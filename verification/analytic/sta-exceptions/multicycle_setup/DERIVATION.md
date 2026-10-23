# Multicycle setup relation with `-end` and `-start` (`multicycle_setup`)

- **Requirement**: `STA-REQ-0260` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `twoclk` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

When a multicycle path with setup multiplier $N$ wins the setup analysis, the setup check uses the
relation:

$$R_{\text{setup}} = D_s + (N - 1) T_s$$

where $T_s = T_C$ (capture clock period) with `-end` (the default) and $T_s = T_L$ (launch clock
period) with `-start`.

### Derivation on `twoclk`

On [`twoclk`](../README.md#twoclk), both `r1 -> r2` (`A -> B`) and `r3 -> r4` (`B -> A`) have rise
arrival `0.629 ns`, fall arrival `0.559 ns`, `t_su = 0.10 ns`, `t_h = 0.05 ns`, and baseline
relations `D_s = 1.0 ns`, `D_h = 0.0 ns` (baseline setup slack `1.0 - 0.1 - 0.629 = 0.2710 ns`,
hold slack `0.559 - 0.05 = 0.5090 ns`):

1. **Period pair `(T_A, T_B) = (4.0, 1.0)` (`c_twoclk41`)**:
   - `ab_s3` (`set_multicycle_path 3 -setup -from A -to B`, default `-end` uses $T_B = 1.0$):
     $R_{\text{setup}} = 1 + 2 \times 1 = 3.0\text{ ns}$, setup slack `3.0 - 0.1 - 0.629 = 2.2710 ns`.
   - `ab_s3_start` (`-start` uses $T_A = 4.0$):
     $R_{\text{setup}} = 1 + 2 \times 4 = 9.0\text{ ns}$, setup slack `9.0 - 0.1 - 0.629 = 8.2710 ns`.
   - `ba_s3` (`-from B -to A`, default `-end` uses $T_A = 4.0$):
     $R_{\text{setup}} = 1 + 2 \times 4 = 9.0\text{ ns}$, setup slack `8.2710 ns`.
   - `ba_s3_start` (`-from B -to A -start` uses $T_B = 1.0$):
     $R_{\text{setup}} = 1 + 2 \times 1 = 3.0\text{ ns}$, setup slack `2.2710 ns`.

2. **Period pair `(T_A, T_B) = (3.0, 2.0)` (`c_twoclk32`)**:
   - `ab_s3` (`-end`, $T_B = 2.0$): $R_{\text{setup}} = 1 + 2 \times 2 = 5.0\text{ ns}$, setup slack `4.2710 ns`.
   - `ab_s3_start` (`-start`, $T_A = 3.0$): $R_{\text{setup}} = 1 + 2 \times 3 = 7.0\text{ ns}$, setup slack `6.2710 ns`.
   - `ba_s3` (`-end`, $T_A = 3.0$): $R_{\text{setup}} = 7.0\text{ ns}$, setup slack `6.2710 ns`.
   - `ba_s3_start` (`-start`, $T_B = 2.0$): $R_{\text{setup}} = 5.0\text{ ns}$, setup slack `4.2710 ns`.

| Period pair | Variant | `A -> B` setup (ns) | `B -> A` setup (ns) |
|---|---|---:|---:|
| `(4, 1)` | `base` | `0.2710` | `0.2710` |
| `(4, 1)` | `ab_s3` (`-end`) | `2.2710` | `0.2710` |
| `(4, 1)` | `ab_s3_start` (`-start`) | `8.2710` | `0.2710` |
| `(4, 1)` | `ba_s3` (`-end`) | `0.2710` | `8.2710` |
| `(4, 1)` | `ba_s3_start` (`-start`) | `0.2710` | `2.2710` |
| `(3, 2)` | `ab_s3` (`-end`) | `4.2710` | `0.2710` |
| `(3, 2)` | `ab_s3_start` (`-start`) | `6.2710` | `0.2710` |
| `(3, 2)` | `ba_s3` (`-end`) | `0.2710` | `6.2710` |
| `(3, 2)` | `ba_s3_start` (`-start`) | `0.2710` | `4.2710` |
