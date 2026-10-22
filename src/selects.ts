// Keep native selects as the source of truth for existing forms and change
// handlers. Render their popups ourselves: WebKitGTK's native option menu
// cannot reliably be themed or given a visible hovered row.
let nextId = 0;
let dismiss: (() => void) | null = null;
export function dismissSelect(): void { dismiss?.(); }

export function enhanceSelects(root: HTMLElement): void {
  for (const select of root.querySelectorAll<HTMLSelectElement>("select:not([data-enhanced])")) {
    select.dataset.enhanced = "true";
    select.id ||= `select-${select.dataset.change ?? select.dataset.delimited ?? select.dataset.excel ?? ++nextId}`;
    const label = select.getAttribute("aria-label") || Array.from(select.labels?.[0]?.childNodes ?? []).filter(n => n.nodeType === 3).map(n => n.textContent?.trim()).join(" ").trim() || "Choose option";
    const wrapper = document.createElement("span"); wrapper.className = "select-control";
    const button = document.createElement("button"); button.type = "button"; button.id = `${select.id}-trigger`; button.className = "select-trigger";
    button.setAttribute("role", "combobox"); button.setAttribute("aria-haspopup", "listbox"); button.setAttribute("aria-expanded", "false");
    for (const attribute of ["aria-describedby", "aria-invalid"]) {
      const value = select.getAttribute(attribute); if (value) button.setAttribute(attribute, value);
    }
    button.disabled = select.disabled;
    button.textContent = select.selectedOptions[0]?.textContent ?? "Choose…";
    button.setAttribute("aria-label", `${label}: ${button.textContent}`);
    select.before(wrapper); wrapper.append(select, button);
    select.classList.add("select-native"); select.tabIndex = -1; select.setAttribute("aria-hidden", "true");
    let popup: HTMLDivElement | null = null, active = 0, typeahead = "", typedAt = 0;
    let pointer: { x: number; y: number } | null = null;
    let options: HTMLOptionElement[] = [], rows: HTMLDivElement[] = [];
    const close = () => {
      popup?.remove(); popup = null;
      pointer = null;
      button.setAttribute("aria-expanded", "false"); button.removeAttribute("aria-activedescendant");
      document.removeEventListener("pointerdown", outside, true);
      window.removeEventListener("resize", close); window.removeEventListener("scroll", scroll, true);
      if (dismiss === close) dismiss = null;
    };
    const outside = (event: Event) => { if (!popup?.contains(event.target as Node) && !wrapper.contains(event.target as Node)) close(); };
    const scroll = (event: Event) => { if (!popup?.contains(event.target as Node)) close(); };
    const activate = (index: number, reveal = true) => {
      if (!options.length) return;
      active = (index + options.length) % options.length;
      rows.forEach((row, i) => row.classList.toggle("is-active", i === active));
      button.setAttribute("aria-activedescendant", rows[active].id);
      if (reveal) rows[active].scrollIntoView({ block: "nearest" });
    };
    const commit = (index: number) => {
      const chosen = options[index]; if (!chosen) return;
      select.value = chosen.value;
      button.textContent = chosen.textContent;
      button.setAttribute("aria-label", `${label}: ${chosen.textContent}`);
      close();
      select.dispatchEvent(new Event("change", { bubbles: true }));
      // A change can replace the form; focus the new control if it survives.
      document.getElementById(button.id)?.focus();
    };
    const show = () => {
      dismissSelect(); if (button.disabled) return;
      button.focus();
      options = Array.from(select.options).filter(o => !o.disabled && !(o.parentElement instanceof HTMLOptGroupElement && o.parentElement.disabled));
      if (!options.length) return;
      popup = document.createElement("div"); popup.className = "select-popup"; popup.id = `${button.id}-list`; popup.setAttribute("role", "listbox"); popup.setAttribute("aria-label", label);
      rows = options.map((option, i) => {
        const row = document.createElement("div"); row.id = `${popup!.id}-${i}`; row.className = "select-option"; row.dataset.value = option.value;
        row.setAttribute("role", "option"); row.setAttribute("aria-selected", String(option.selected));
        const text = document.createElement("span"); text.textContent = option.textContent;
        const check = document.createElement("span"); check.className = "select-check"; check.textContent = option.selected ? "✓" : ""; check.setAttribute("aria-hidden", "true");
        row.append(text, check); row.addEventListener("pointermove", event => { pointer = { x: event.clientX, y: event.clientY }; activate(i, false); });
        row.addEventListener("pointerdown", e => e.preventDefault()); row.addEventListener("click", () => commit(i));
        popup!.append(row); return row;
      });
      document.body.append(popup);
      popup.addEventListener("pointerleave", () => { pointer = null; });
      popup.addEventListener("scroll", () => {
        if (!pointer) return;
        const row = document.elementFromPoint(pointer.x, pointer.y)?.closest<HTMLDivElement>(".select-option");
        const index = row ? rows.indexOf(row) : -1; if (index >= 0) activate(index, false);
      });
      const rect = button.getBoundingClientRect(), gap = 6, below = window.innerHeight - rect.bottom - 12, above = rect.top - 12;
      const upward = below < 200 && above > below;
      popup.style.width = `${Math.min(Math.max(rect.width, 220), window.innerWidth - 16)}px`;
      popup.style.maxHeight = `${Math.max(80, Math.min(320, upward ? above : below))}px`;
      popup.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - popup.offsetWidth - 8))}px`;
      popup.style.top = `${upward ? Math.max(8, rect.top - popup.offsetHeight - gap) : rect.bottom + gap}px`;
      button.setAttribute("aria-expanded", "true"); button.setAttribute("aria-controls", popup.id);
      dismiss = close; activate(Math.max(0, options.findIndex(o => o.selected)));
      document.addEventListener("pointerdown", outside, true); window.addEventListener("resize", close); window.addEventListener("scroll", scroll, true);
    };
    button.addEventListener("click", () => popup ? close() : show());
    button.addEventListener("blur", () => { close(); });
    button.addEventListener("keydown", (event) => {
      if (["ArrowDown", "ArrowUp", "Home", "End", "PageUp", "PageDown", "Enter", " ", "Escape"].includes(event.key)) {
        event.preventDefault(); event.stopPropagation();
        pointer = null;
        if (event.key === "Escape") { close(); return; }
        if (event.key === "Enter" || event.key === " ") { if (popup) commit(active); else show(); return; }
        const wasOpen = !!popup; if (!popup) show();
        if (event.key === "Home") activate(0);
        else if (event.key === "End") activate(options.length - 1);
        else if (event.key === "PageUp" || event.key === "PageDown") activate(Math.max(0, Math.min(options.length - 1, active + (event.key === "PageDown" ? 8 : -8))));
        else if (wasOpen) activate(active + (event.key === "ArrowDown" ? 1 : -1));
      } else if (event.key === "Tab") close();
      else if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
        event.preventDefault(); event.stopPropagation();
        typeahead = Date.now() - typedAt > 700 ? event.key : typeahead + event.key; typedAt = Date.now();
        if (!popup) show();
        const index = options.findIndex(o => o.textContent?.trim().toLocaleLowerCase().startsWith(typeahead.toLocaleLowerCase()));
        if (index >= 0) activate(index);
      }
    });
  }
}
