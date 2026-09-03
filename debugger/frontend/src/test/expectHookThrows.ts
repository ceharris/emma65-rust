import { renderHook } from "@testing-library/react";
import { vi } from "vitest";

/**
 * Renders a hook expected to throw during mount (e.g. a `useXContext` guard
 * against a missing provider). With no error boundary in the tree, React
 * dev mode both logs the error via `console.error` and dispatches a DOM
 * "error" event to report it the same way a browser would — jsdom then logs
 * that event as an uncaught error too — even though the caller (via
 * `.toThrow()`) handles the exception fine. Suppress both so intentional
 * throw tests don't spam CI/test output with noise that looks like a real
 * failure.
 */
export function expectHookThrows(hook: () => unknown): () => void {
  return () => {
    const suppressReportedError = (e: Event) => e.preventDefault();
    window.addEventListener("error", suppressReportedError);
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      renderHook(hook);
    } finally {
      window.removeEventListener("error", suppressReportedError);
      consoleError.mockRestore();
    }
  };
}
