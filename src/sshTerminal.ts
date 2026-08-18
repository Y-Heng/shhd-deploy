import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";

export function createSshTerminal() {
  const terminal = new Terminal({
    fontFamily: "Cascadia Code, Consolas, Menlo, Monaco, monospace",
    fontSize: 14,
    cursorBlink: false,
    cursorStyle: "block",
    scrollback: 5000,
    rightClickSelectsWord: false,
    theme: {
      background: "#1e1e2e",
      foreground: "#3dd68c",
      cursor: "#3dd68c",
      cursorAccent: "#1e1e2e",
      selectionBackground: "#4a7ec8",
      selectionInactiveBackground: "#355a8c",
      selectionForeground: "#eef4ff",
      black: "#1e1e2e",
      red: "#f38ba8",
      green: "#3dd68c",
      yellow: "#f9e2af",
      blue: "#89b4fa",
      magenta: "#e84e7f",
      cyan: "#89dceb",
      white: "#3dd68c",
      brightBlack: "#585b70",
      brightRed: "#f38ba8",
      brightGreen: "#3dd68c",
      brightYellow: "#f9e2af",
      brightBlue: "#89b4fa",
      brightMagenta: "#e84e7f",
      brightCyan: "#89dceb",
      brightWhite: "#3dd68c",
    },
  });
  const fitAddon = new FitAddon();
  terminal.loadAddon(fitAddon);
  return { terminal, fitAddon };
}

export function parsePopoutHash() {
  const raw = window.location.hash || "";
  if (!raw.startsWith("#/popout-term")) return null;
  const queryIndex = raw.indexOf("?");
  const query = new URLSearchParams(queryIndex >= 0 ? raw.slice(queryIndex + 1) : "");
  const sessionId = query.get("sessionId") || "";
  if (!sessionId) return null;
  return {
    sessionId,
    title: query.get("title") || "SSH",
    serverId: query.get("serverId") || "",
  };
}
