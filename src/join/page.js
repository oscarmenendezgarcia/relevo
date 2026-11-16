// The invitation page (/join/<code>): copy buttons and the system tabs. Works without it too.
for (const button of document.querySelectorAll("[data-copy]")) {
  const label = button.textContent;
  button.addEventListener("click", async () => {
    const text = document.getElementById(button.dataset.copy);
    try {
      await navigator.clipboard.writeText(text.textContent);
    } catch {
      // No clipboard API (a page over plain http): select it, and copy the old way.
      const range = document.createRange();
      range.selectNodeContents(text);
      getSelection().removeAllRanges();
      getSelection().addRange(range);
      document.execCommand("copy");
    }
    button.textContent = "Copied";
    setTimeout(() => { button.textContent = label; }, 1500);
  });
}
const tabs = document.querySelectorAll("[role=tab]");
for (const tab of tabs) {
  tab.addEventListener("click", () => {
    for (const other of tabs) {
      const on = other === tab;
      other.setAttribute("aria-selected", String(on));
      document.getElementById(other.getAttribute("aria-controls")).hidden = !on;
    }
  });
}
