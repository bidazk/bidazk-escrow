# bidazk-escrow

Soroban smart contract for Bidazk escrow on Stellar. Buyer pays into the
contract; it releases to the seller on confirmation or after an inspection
window, refunds the buyer if the seller misses the delivery deadline, and lets a
referee split disputed deals. Design and rules: [DESIGN.md](DESIGN.md).

## Testnet deployments

| Instance | Contract | Arbiter |
|---|---|---|
| CLI demo | `CDUVEHVLAVIWAFC3YM35BW74DPEFG7DTFJHQQZWPMXOG4BUFZCAFTBA7` | `bidazk-arbiter` (private) |
| Web demo (bidazk.xyz/demo) | `CCTJT2ETAK42HFZDCGJTSMEGX3D2OOX2XAAHLWUQAOPHHMCP5ANAGY2X` | `web-demo-arbiter` (key public in the web bundle on purpose) |

Token: native XLM SAC `CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC`.
Testnet resets periodically; after a reset, redeploy and update
`bidazk-web/lib/escrow/config.ts`.

## Commands

```sh
cargo test                     # 34 tests, every rule in DESIGN.md
stellar contract build         # → target/wasm32v1-none/release/bidazk_escrow.wasm

stellar contract deploy \
  --wasm target/wasm32v1-none/release/bidazk_escrow.wasm \
  --source bidazk-arbiter --network testnet --alias bidazk-escrow \
  -- --arbiter bidazk-arbiter \
     --token CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC
```

### CLI walkthrough (amounts in stroops: 1 XLM = 10,000,000)

```sh
stellar contract invoke --id bidazk-escrow --source demo-buyer --network testnet -- \
  create --buyer demo-buyer --seller demo-seller --item 1 --amount 100000000 \
  --delivery_window 300 --inspection_period 120
stellar contract invoke --id bidazk-escrow --source demo-seller --network testnet -- mark_delivered --deal_id 0
stellar contract invoke --id bidazk-escrow --source demo-buyer  --network testnet -- confirm --deal_id 0
stellar contract invoke --id bidazk-escrow --source demo-buyer  --network testnet --send=no -- get_deal --deal_id 0
```
