import { useState, type ReactNode } from "react";
import { House } from "lucide-react";
import { cn } from "@/lib/utils";
import { Home } from "./components/Home";
import { Separator } from "@/components/ui/separator";

type Tab = "home";

const TABS: { id: Tab; label: string; icon: ReactNode }[] = [
  { id: "home", label: "Home", icon: <House size={15} /> },
];

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("home");
  const [gamePath, setGamePath] = useState("");

  return (
    <div className="flex h-screen overflow-hidden bg-background text-foreground">
      {/* Sidebar */}
      <nav className="flex w-[210px] min-w-[210px] flex-col overflow-y-auto border-r border-border bg-card py-4">
        <div className="flex items-center gap-2.5 px-4 pb-4">
          <span className="text-3xl leading-none">🐽</span>
          <div>
            <div className="text-[15px] font-bold">Oinkers Editor</div>
            <div className="text-[11px] text-muted-foreground">Marvel Rivals Mod Tool</div>
          </div>
        </div>

        <Separator className="mb-2" />

        <ul className="flex flex-1 flex-col gap-0.5 px-2">
          {TABS.map((t) => (
            <li key={t.id}>
              <button
                onClick={() => setActiveTab(t.id)}
                className={cn(
                  "flex w-full items-center gap-2.5 rounded-sm px-2.5 py-2 text-[13px] text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground",
                  activeTab === t.id && "bg-secondary font-semibold text-foreground",
                )}
              >
                {t.icon}
                {t.label}
              </button>
            </li>
          ))}
        </ul>

        {gamePath && (
          <>
            <Separator className="mb-3" />
            <div className="px-4">
              <span className="block text-[10px] uppercase tracking-widest text-muted-foreground">Game Root</span>
              <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-xs text-foreground" title={gamePath}>
                {gamePath.split(/[/\\]/).pop()}
              </span>
            </div>
          </>
        )}
      </nav>

      {/* Content */}
      <main className="flex-1 overflow-y-auto p-6 bg-background">
        {activeTab === "home" && (
          <Home gamePath={gamePath} setGamePath={setGamePath} />
        )}
      </main>
    </div>
  );
}

export default App;
