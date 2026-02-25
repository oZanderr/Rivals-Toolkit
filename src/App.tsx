import { useState } from "react";
import "./App.css";
import { Home } from "./components/Home";

type Tab = "home";

const TABS: { id: Tab; label: string; icon: string }[] = [
  { id: "home", label: "Home", icon: "🏠" },
];

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("home");
  const [gamePath, setGamePath] = useState("");

  return (
    <div className="app">
      {/* Sidebar */}
      <nav className="sidebar">
        <div className="sidebar-header">
          <span className="app-logo">🐽</span>
          <div>
            <div className="app-name">Oinkers Editor</div>
            <div className="app-sub">Marvel Rivals Mod Tool</div>
          </div>
        </div>

        <ul className="nav-list">
          {TABS.map((t) => (
            <li key={t.id}>
              <button
                className={`nav-item ${activeTab === t.id ? "active" : ""}`}
                onClick={() => setActiveTab(t.id)}
              >
                <span className="nav-icon">{t.icon}</span>
                <span>{t.label}</span>
              </button>
            </li>
          ))}
        </ul>

        {gamePath && (
          <div className="sidebar-footer">
            <span className="footer-label">Game Root</span>
            <span className="footer-path" title={gamePath}>
              {gamePath.split(/[/\\]/).pop()}
            </span>
          </div>
        )}
      </nav>

      {/* Content */}
      <main className="content">
        {activeTab === "home" && (
          <Home gamePath={gamePath} setGamePath={setGamePath} />
        )}
      </main>
    </div>
  );
}

export default App;
