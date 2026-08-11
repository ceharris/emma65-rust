import { useEffect } from "react";

/**
 * Phase 0 spike only (issue #379). Logs mount/unmount to the devtools
 * console so a human can confirm whether dockview actually unmounts a tab's
 * React component when it becomes inactive, or merely detaches its
 * container from the document while leaving the component mounted (the
 * "critical" spike question 2). Switch tabs in the spike window and watch
 * the console: no "unmounted" log on tab-away means the component survives.
 */
export default function SpikeMountLog({ label }: { label: string }) {
  useEffect(() => {
    console.log(`[spike] ${label} mounted`);
    return () => console.log(`[spike] ${label} unmounted`);
  }, [label]);

  return null;
}
