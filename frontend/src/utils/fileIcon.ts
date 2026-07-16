import {
  File as FileIcon,
  FileText, FileCode, Archive, Terminal, Database, BookOpen,
  Package, Table, Cpu,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";

// Extension → MIME type for the file kinds Pollis previews or uploads. Single
// source of truth for the composer (ChatInput, full map) and R2 public-file
// downloads (image-only view below).
export const EXT_MIME: Record<string, string> = {
  // Images
  jpg: "image/jpeg", jpeg: "image/jpeg", png: "image/png",
  gif: "image/gif", webp: "image/webp", svg: "image/svg+xml", avif: "image/avif",
  // Video
  mp4: "video/mp4", mov: "video/quicktime", webm: "video/webm",
  // Audio
  mp3: "audio/mpeg", wav: "audio/wav", ogg: "audio/ogg", m4a: "audio/mp4",
  flac: "audio/flac", opus: "audio/opus", aac: "audio/aac",
  // Documents / archives
  pdf: "application/pdf", zip: "application/zip",
};

// Image-only subset of EXT_MIME. R2 public downloads (avatars, group icons)
// only ever serve images, so the extension lookup stays narrow.
export const IMAGE_EXT_MIME: Record<string, string> = Object.fromEntries(
  Object.entries(EXT_MIME).filter(([, mime]) => mime.startsWith("image/")),
);

// Best-effort MIME type for a filename, falling back to a generic binary type.
export function mimeFromName(name: string): string {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return EXT_MIME[ext] ?? "application/octet-stream";
}

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
