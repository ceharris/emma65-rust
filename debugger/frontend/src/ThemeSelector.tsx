import { useTheme, ThemeMode } from "./ThemeContext";
import "./styles/toolbar.scss";

const OPTIONS: { mode: ThemeMode; label: string }[] = [
  { mode: "auto", label: "Auto" },
  { mode: "dark", label: "Dark" },
  { mode: "light", label: "Light" },
];

/** Segmented Auto/Dark/Light theme control shown in the app toolbar. */
export default function ThemeSelector() {
  const { mode, setTheme } = useTheme();

  return (
    <div className="theme-toggle" role="group" aria-label="Theme">
      {OPTIONS.map((option) => (
        <button
          key={option.mode}
          type="button"
          className="theme-toggle-btn"
          aria-pressed={mode === option.mode}
          onClick={() => setTheme(option.mode)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}
