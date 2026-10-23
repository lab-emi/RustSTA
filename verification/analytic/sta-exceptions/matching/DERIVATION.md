# Conjunctive options, disjunctive object lists, and mixed lists (`matching`)

- **Requirement**: `STA-REQ-0255` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `xbar, reconv` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

An exception matches a path when every option (`-from`, `-through`, `-to`) matches the path
(conjunction across options) and at least one object in each option's list matches the path
(disjunction within an option). When an option mixes clocks with design objects, it counts as
naming a design object for paths matched via a design object and as naming a clock for paths
matched only via a clock.

### Derivation on `xbar` and `reconv`

```sdc
# On xbar:
# fromto_and: conjunction of -from r1/CK AND -to r4/D -> disables only s14
set_false_path -from [get_pins r1/CK] -to [get_pins r4/D]

# lists_or: disjunction inside -from {r1/CK r2/CK} AND -to r4/D -> disables s14 and s24
set_false_path -from [get_pins {r1/CK r2/CK}] -to [get_pins r4/D]

# thr_list: disjunction inside -through {g1/A g2/B} -> disables s13 and s24
set_false_path -through [get_pins {g1/A g2/B}]

# On reconv:
# s_mixed: mixed -from [list [get_clocks clk] [get_pins r1/CK]] (N = 3) vs -to [get_pins r3/D] (N = 2)
# On r1 -> r3, the mixed list matches via pin r1/CK (specificity 16 > 8), so N = 3 wins!
# On r2 -> r3, the mixed list matches only via clock clk (specificity 2 < 8), so N = 2 wins!
set_multicycle_path 3 -setup -from [list [get_clocks clk] [get_pins r1/CK]]
set_multicycle_path 2 -setup -to [get_pins r3/D]
```

| Circuit / Variant | Slacks (ns) | Explanation |
|---|---|---|
| `xbar` / `fromto_and` | `s13 = 1.2600, s14 = --, s23 = 1.2600, s24 = 1.1300` | Only `r1 -> r4` matches both `-from` and `-to` |
| `xbar` / `lists_or` | `s13 = 1.2600, s14 = --, s23 = 1.2600, s24 = --` | Either `r1/CK` or `r2/CK` to `r4/D` matches |
| `xbar` / `thr_list` | `s13 = --, s14 = 1.1300, s23 = 1.2600, s24 = --` | Passes through `g1/A` (`s13`) or `g2/B` (`s24`) |
| `reconv` / `s_mixed` | `s1 = 4.8370, h1 = -3.0948, s2 = 3.2710, h2 = -1.4910` | `r1->r3` gets `N=3` (pin spec `16 > 8`); `r2->r3` gets `N=2` (`-to` pin `8 > 2`) |
