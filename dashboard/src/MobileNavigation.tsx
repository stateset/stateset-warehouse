import { useEffect, useRef, useState } from "react";
import { Box, CircleHelp, Grid2X2, LogOut, Search, X } from "lucide-react";
import type { Tab } from "./uiTypes";

type NavItem = { id: Tab; title: string; icon: typeof Box };

export default function MobileNavigation({
  items,
  groups,
  tab,
  navigate,
  openIssues,
  onHelp,
  onLogout,
}: {
  items: NavItem[];
  groups: { title: string; items: Tab[] }[];
  tab: Tab;
  navigate: (tab: Tab) => void;
  openIssues: number;
  onHelp: () => void;
  onLogout: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const dialog = useRef<HTMLDialogElement>(null);
  const primary: Tab[] = ["overview", "inventory", "scanner", "fulfillment"];

  useEffect(() => {
    if (open) dialog.current?.showModal();
    else dialog.current?.close();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const previous = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const desktop = window.matchMedia("(min-width: 681px)");
    const closeOnDesktop = () => {
      if (desktop.matches) setOpen(false);
    };
    desktop.addEventListener("change", closeOnDesktop);
    return () => {
      document.body.style.overflow = previous;
      desktop.removeEventListener("change", closeOnDesktop);
    };
  }, [open]);

  const select = (next: Tab) => {
    setOpen(false);
    navigate(next);
  };
  const matches = items.filter((item) =>
    item.title.toLowerCase().includes(query.trim().toLowerCase()),
  );

  return (
    <>
      <nav className="mobile-navigation" aria-label="Primary navigation">
        {primary.map((id) => {
          const item = items.find((item) => item.id === id)!;
          return (
            <button
              key={id}
              aria-label={item.title}
              aria-current={tab === id ? "page" : undefined}
              onClick={() => select(id)}
            >
              <item.icon size={20} aria-hidden="true" />
              <span>{item.title}</span>
            </button>
          );
        })}
        <button
          aria-label="All tools"
          aria-haspopup="dialog"
          aria-expanded={open}
          className={!primary.includes(tab) ? "current" : ""}
          onClick={() => {
            setQuery("");
            setOpen(true);
          }}
        >
          <Grid2X2 size={20} aria-hidden="true" />
          <span>All tools</span>
          {openIssues > 0 && (
            <i
              className="mobile-attention-dot"
              aria-label={`${openIssues} open issues`}
            />
          )}
        </button>
      </nav>
      <dialog
        ref={dialog}
        className="tools-dialog"
        aria-labelledby="tools-title"
        onCancel={() => setOpen(false)}
        onClose={() => setOpen(false)}
      >
        <div className="tools-heading">
          <div>
            <span className="eyebrow">YOUR WORKSPACE</span>
            <h2 id="tools-title">All tools</h2>
          </div>
          <button
            className="icon-btn"
            aria-label="Close tools"
            onClick={() => setOpen(false)}
          >
            <X size={20} />
          </button>
        </div>
        <label className="search-field tools-search">
          <Search size={17} aria-hidden="true" />
          <input
            aria-label="Find a tool"
            placeholder="Find a tool…"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <div className="tools-groups">
          {groups.map((group) => {
            const visible = matches.filter((item) =>
              group.items.includes(item.id),
            );
            if (!visible.length) return null;
            return (
              <section key={group.title} aria-label={group.title}>
                <h3>{group.title}</h3>
                <div className="tools-grid">
                  {visible.map((item) => (
                    <button
                      key={item.id}
                      aria-label={item.title}
                      aria-current={tab === item.id ? "page" : undefined}
                      onClick={() => select(item.id)}
                    >
                      <item.icon size={20} aria-hidden="true" />
                      <span>{item.title}</span>
                      {item.id === "exceptions" && openIssues > 0 && (
                        <b>{openIssues}</b>
                      )}
                    </button>
                  ))}
                </div>
              </section>
            );
          })}
          {!matches.length && (
            <p className="tools-empty" role="status">
              No tools match “{query}”. Try another name.
            </p>
          )}
        </div>
        <div className="tools-footer">
          <button
            className="btn"
            onClick={() => {
              setOpen(false);
              onHelp();
            }}
          >
            <CircleHelp size={16} />
            Workspace guide
          </button>
          <button
            className="btn"
            onClick={() => {
              setOpen(false);
              onLogout();
            }}
          >
            <LogOut size={16} />
            Sign out
          </button>
        </div>
      </dialog>
    </>
  );
}
