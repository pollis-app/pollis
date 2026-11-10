// The composer's paste path (#1794): files the paste event carries, else one bounded OS clipboard read.

export interface PasteItem {
  kind: string;
  getAsFile(): File | null;
}

/** Files on the paste event; a zero-byte file (an offer the webview could not read) is not a file. */
export function pastedFiles(items: ArrayLike<PasteItem>): File[] {
  const files: File[] = [];
  for (let i = 0; i < items.length; i++) {
    if (items[i].kind !== "file") {
      continue;
    }
    const file = items[i].getAsFile();
    if (file && file.size > 0) {
      files.push(file);
    }
  }
  return files;
}

export interface OsPasteDeps {
  readFiles: () => Promise<string[]>;
  readImage: () => Promise<Uint8Array<ArrayBuffer> | null>;
  onPaths: (paths: string[]) => void;
  onFile: (file: File) => void;
}

export type OsPasteResult = "paths" | "image" | "none" | "busy";

/**
 * Read files, then an image, from the OS clipboard. One read at a time: a second
 * Ctrl+V while the first waits on a slow clipboard owner is dropped, not queued.
 * Never rejects; an unreadable clipboard is "none".
 */
export function createOsPasteReader(deps: () => OsPasteDeps): () => Promise<OsPasteResult> {
  let inFlight = false;
  return async () => {
    if (inFlight) {
      return "busy";
    }
    inFlight = true;
    try {
      const { readFiles, readImage } = deps();
      const paths = await readFiles().catch(() => [] as string[]);
      // Callbacks fetched after the await: the composer's state moved on meanwhile.
      if (paths.length > 0) {
        deps().onPaths(paths);
        return "paths";
      }
      const bytes = await readImage().catch(() => null);
      if (bytes && bytes.byteLength > 0) {
        deps().onFile(new File([bytes], "pasted-image.png", { type: "image/png" }));
        return "image";
      }
      return "none";
    } finally {
      inFlight = false;
    }
  };
}
