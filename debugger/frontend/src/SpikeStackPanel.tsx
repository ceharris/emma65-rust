import StackPanel from "./StackPanel";
import SpikeTicker from "./SpikeTicker";

/**
 * Phase 0 spike only (issue #379). The real, unmodified `StackPanel` mounted
 * both as a dockview panel (attached) and inside the standalone detached
 * window (`stack-detached.tsx`) — the same component in both places proves
 * `invoke()`-backed panels behave identically in a dynamically created
 * window (spike question 4). `SpikeTicker` rides along to prove out the
 * `emit_to`-retargeting mechanism (spike question 5).
 */
export default function SpikeStackPanel() {
  return (
    <div>
      <StackPanel />
      <SpikeTicker />
    </div>
  );
}
