| arm | rung | blocks | `sustained` (both ceilings) | `sustained_primary` (lag rule) | `service_ceiling_reported_windows` (sum) | lag p99 us per block | verdicts differ |
|---|---|---|---|---|---|---|---|
| flashdb-declared | 50 | 5 | 5/5 | 5/5 | 0 | 1038, 3894, 1055, 846, 883 | 0 |
| sqlite | 50 | 5 | 5/5 | 5/5 | 0 | 375, 577, 2544, 862, 515 | 0 |
| postgres | 50 | 5 | 5/5 | 5/5 | 0 | 1289, 1142, 385, 205, 266 | 0 |
| flashdb-declared | 100 | 5 | 5/5 | 5/5 | 0 | 857, 371, 2288, 1401, 451 | 0 |
| sqlite | 100 | 5 | 5/5 | 5/5 | 0 | 251, 308, 722, 231, 192 | 0 |
| postgres | 100 | 5 | 5/5 | 5/5 | 0 | 264, 290, 775, 171, 257 | 0 |
| flashdb-declared | 200 | 5 | 3/5 | 3/5 | 0 | 7272, 30430, 33747, 3088, 8389 | 0 |
| sqlite | 200 | 5 | 5/5 | 5/5 | 0 | 79, 33, 170, 164, 151 | 0 |
| postgres | 200 | 5 | 5/5 | 5/5 | 0 | 968, 274, 162, 906, 1731 | 0 |
