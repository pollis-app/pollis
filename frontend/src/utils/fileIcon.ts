import {
  File as FileIcon,
  FileText, FileCode, Archive, Terminal, Database, BookOpen,
  Package, Table, Cpu,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";

export function getFileIcon(filename: string): LucideIcon {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "md":
    case "mdx":
      return BookOpen;
    case "js":
    case "ts":
    case "jsx":
    case "tsx":
    case "py":
    case "rs":
    case "go":
    case "java":
    case "cpp":
    case "c":
    case "h":
    case "rb":
    case "php":
    case "swift":
    case "kt":
    case "css":
    case "html":
    case "xml":
    case "yaml":
    case "yml":
    case "toml":
    case "ini":
    case "conf":
    case "json":
    case "jsonc":
      return FileCode;
    case "doc":
    case "docx":
    case "txt":
    case "rtf":
    case "odt":
    case "pdf":
    case "ppt":
    case "pptx":
    case "key":
    case "odp":
      return FileText;
    case "zip":
    case "tar":
    case "gz":
    case "rar":
    case "7z":
    case "bz2":
    case "xz":
      return Archive;
    case "sh":
    case "bash":
    case "zsh":
    case "fish":
    case "bat":
    case "cmd":
    case "ps1":
      return Terminal;
    case "exe":
    case "dmg":
    case "pkg":
    case "deb":
    case "rpm":
    case "msi":
    case "appimage":
      return Package;
    case "sql":
    case "db":
    case "sqlite":
    case "sqlite3":
      return Database;
    case "csv":
    case "tsv":
    case "xls":
    case "xlsx":
    case "ods":
      return Table;
    case "wasm":
    case "bin":
    case "elf":
    case "so":
    case "dll":
      return Cpu;
    default:
      return FileIcon;
  }
}
