import { memo, useEffect, useMemo, useRef, useState } from "react";
import { Search, X } from "lucide-react";

export interface CommandItem {
  id: string;
  title: string;
  detail?: string;
  shortcut?: string;
  run: () => void | Promise<void>;
}

function CommandPaletteView({
  open,
  commands,
  onClose,
}: {
  open: boolean;
  commands: CommandItem[];
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return commands;
    return commands.filter((command) => `${command.title} ${command.detail ?? ""}`.toLowerCase().includes(needle));
  }, [commands, query]);

  useEffect(() => {
    if (!open) return;
    setQuery("");
    setActiveIndex(0);
    window.setTimeout(() => inputRef.current?.focus(), 20);
  }, [open]);

  useEffect(() => {
    setActiveIndex((index) => Math.min(index, Math.max(filtered.length - 1, 0)));
  }, [filtered.length]);

  if (!open) return null;

  async function run(command: CommandItem) {
    onClose();
    await command.run();
  }

  return (
    <div className="command-backdrop" onMouseDown={onClose}>
      <section className="command-palette" onMouseDown={(event) => event.stopPropagation()}>
        <div className="command-search">
          <Search size={17} />
          <input
            ref={inputRef}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                onClose();
              } else if (event.key === "ArrowDown") {
                event.preventDefault();
                setActiveIndex((index) => Math.min(index + 1, Math.max(filtered.length - 1, 0)));
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                setActiveIndex((index) => Math.max(index - 1, 0));
              } else if (event.key === "Enter" && filtered[activeIndex]) {
                event.preventDefault();
                void run(filtered[activeIndex]);
              }
            }}
            placeholder="Search commands"
          />
          <button className="icon-button" title="Close" onClick={onClose}>
            <X size={16} />
          </button>
        </div>
        <div className="command-list">
          {filtered.map((command, index) => (
            <button
              key={command.id}
              className={index === activeIndex ? "command-item active" : "command-item"}
              onMouseEnter={() => setActiveIndex(index)}
              onClick={() => void run(command)}
            >
              <span>
                <strong>{command.title}</strong>
                {command.detail && <small>{command.detail}</small>}
              </span>
              {command.shortcut && <kbd>{command.shortcut}</kbd>}
            </button>
          ))}
          {!filtered.length && <div className="command-empty">No command found</div>}
        </div>
      </section>
    </div>
  );
}

export const CommandPalette = memo(CommandPaletteView);
