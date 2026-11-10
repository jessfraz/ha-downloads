import { mountAppShell } from "./app-kit.js";

const header = document.querySelector(".app-header");
const footer = document.getElementById("app-footer");
const refresh = document.querySelector(".refresh-controls");
if (!(header instanceof HTMLElement) || !(footer instanceof HTMLElement) || !(refresh instanceof HTMLElement)) {
  throw new Error("The app shell is unavailable.");
}
export const shell = mountAppShell({
  name: "Downloads",
  header,
  footer,
  sourceUrl: "https://github.com/jessfraz/ha-downloads",
  brand: [...header.querySelectorAll("#app-brand > svg")],
  actions: [refresh],
});
shell.version.id = "app-version";
shell.status.id = "service-status";
