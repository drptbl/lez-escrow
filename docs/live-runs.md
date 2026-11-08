# Live runs on the LEZ testnet

Both runs went through `cli/` against program `4gCJP2bc5YBCh3saEzudfTmNYZ4uZNDj8fUea4wsHrEM` on `https://testnet.lez.logos.co/`, on 2026-11-08. A separate payer, `DoyatUQC8XwBdRYFRNViv8RLFfupzCMCVWReVXgMQquD`, co-signed every transaction and paid the fees.

| Role | Account |
| --- | --- |
| Buyer | `DKZmzyVG8sccQKQPGaFoZr8wi6rMGgMwJiB77YCPZjsQ` |
| Provider | `4n2DCJMYveciCLAvfcR1nWvMFbr7f8zSc7KGaG7YMpLK` |

## Open, deliver, claim

Job ref `10e77eb5e89b3e325d566683fb6236c274e169623a0e6d14f8261cc99687d0b3`, job account `7kAfedYyXxuMUasPAVQwacdpqCMawi978CgoGD5NoDAm`, amount 1,000,000 lepta. The result hash is the SHA-256 of `lez-escrow live run A result`.

| Step | Block | Transaction | Job holds | State |
| --- | --- | --- | --- | --- |
| `Open` | 79132 | `b327d9ac123747fc4fbcc0be05b2a13a45b7856ada8c46fbba8254eae767fe5e` | 1,000,000 | `Open` |
| `Deliver` | 79134 | `e26dc13d55ce6650614419534ad35996c58df1aca0bb2ea956616ceb03a6c6da` | 1,000,000 | `Delivered` |
| `Claim` | 79135 | `542d32bc2132a129ab4655b2e3e0f6b9aa8e9c97031d14440d4cb5e687fb11ee` | 0 | `Claimed` |

The provider's balance went from 0 to 1,000,000.

## Open, timeout, refund

Job ref `78d81ff4ce0688a6e939be631de93c43bc874b32461509201b6f1570cba29651`, job account `3Nuu8iVmftVXVC9Q2L17vbE2TgBc1mbrZHEz5pHUvfiK`, amount 1,000,000 lepta, deadline `1794162065000` (18:21:05 UTC). The provider never delivered.

| Step | Block | Transaction | Job holds | State |
| --- | --- | --- | --- | --- |
| `Open` | 79130 | `7eb74c9ef15bef630281b1fbdddabc59721095efdb25f40b4bffea626d57d39e` | 1,000,000 | `Open` |
| `Refund` | 79141 | `3e45caf1807aab147dc2fea5375b2033a9df01b7710ac3db6dcf66dbee3dd835` | 0 | `Refunded` |

Block 79141 is timestamped 18:23:23 UTC, 138 seconds after the deadline. The buyer started with 2,100,000 and ended with 1,100,000: job A's 1,000,000 went to the provider and job B's came back.
