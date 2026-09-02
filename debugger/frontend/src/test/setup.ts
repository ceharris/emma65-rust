import "@testing-library/jest-dom/vitest";
// Registers the @tauri-apps/api mocks (via vi.mock) before any test file's
// own imports are resolved — see tauriMock.ts's module comment.
import "./tauriMock";
