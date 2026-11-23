Changed
- `scripts/gate.sh --evo` (or `GATE_EVO=1`) runs the heavy cargo steps on EVO through `evo-gate-log` with the CI toolchain pins and keeps fmt and `cargo tree` on the Mac; an unreachable EVO or an `evo-gate-log` refusal fails the gate (#264).
