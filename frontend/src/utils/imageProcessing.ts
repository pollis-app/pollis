export interface ResizeOptions {
  maxSize?: number; // Max width/height in pixels (default: 1024)
  quality?: number; // JPEG quality 0-1 (default: 0.85)
  outputFormat?: 'image/jpeg' | 'image/png' | 'image/webp'; // Default: 'image/jpeg'
}

/**
 * Resize and optimize an image file for upload
 *
 * @param file - Original image file
 * @param options - Resize and compression options
 * @returns Optimized image file
 */
export async function resizeImage(
  file: File,
  options: ResizeOptions = {}
): Promise<File> {
  const {
    maxSize = 1024,
    quality = 0.85,
    outputFormat = 'image/jpeg',
  } = options;

  // GIFs pass through untouched: canvas flattens animation.
  if (file.type === 'image/gif') {
    return file;
  }

  return new Promise((resolve, reject) => {
    const img = new Image();
    const canvas = document.createElement('canvas');
    const ctx = canvas.getContext('2d');

    if (!ctx) {
      reject(new Error('Failed to get canvas context'));
      return;
    }

    img.onload = () => {
      let width = img.width;
      let height = img.height;

      if (width > maxSize || height > maxSize) {
        if (width > height) {
          height = (height / width) * maxSize;
          width = maxSize;
        } else {
          width = (width / height) * maxSize;
          height = maxSize;
        }
      }

      canvas.width = width;
      canvas.height = height;
      ctx.drawImage(img, 0, 0, width, height);

      canvas.toBlob(
        (blob) => {
          if (!blob) {
            reject(new Error('Failed to compress image'));
            return;
          }

          const extension = outputFormat.split('/')[1];
          const fileName = file.name.replace(/\.[^/.]+$/, `.${extension}`);

          const optimizedFile = new File([blob], fileName, {
            type: outputFormat,
            lastModified: Date.now(),
          });
          resolve(optimizedFile);
        },
        outputFormat,
        quality
      );
    };

    img.onerror = () => reject(new Error('Failed to load image'));
    img.src = URL.createObjectURL(file);
  });
}

/** Blurhash and dimensions from an image URL via a tiny canvas; null on failure. */
export async function blurhashFromUrl(
  url: string,
): Promise<{ hash: string; width: number; height: number } | null> {
  const { encode } = await import('blurhash');
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => {
      const W = img.naturalWidth;
      const H = img.naturalHeight;
      if (!W || !H) { resolve(null); return; }
      const bw = 32;
      const bh = Math.max(1, Math.round(32 * (H / W)));
      const canvas = document.createElement('canvas');
      canvas.width = bw;
      canvas.height = bh;
      const ctx = canvas.getContext('2d');
      if (!ctx) { resolve(null); return; }
      ctx.drawImage(img, 0, 0, bw, bh);
      const { data } = ctx.getImageData(0, 0, bw, bh);
      try {
        const hash = encode(data, bw, bh, 4, 3);
        resolve({ hash, width: W, height: H });
      } catch {
        resolve(null);
      }
    };
    img.onerror = () => resolve(null);
    img.src = url;
  });
}

export type CapturedPoster = {
  /** JPEG object URL the caller must revoke. */
  url: string;
  /** The same JPEG, for the send path (#1837). */
  blob: Blob;
  /** Seconds, or 0 when unknown. */
  duration: number;
};

/** True when nothing was painted: every sampled pixel is fully transparent. */
function drewNothing(ctx: CanvasRenderingContext2D, w: number, h: number): boolean {
  const { data } = ctx.getImageData(0, 0, w, h);
  // Every 64th pixel is plenty: a real frame is opaque everywhere.
  for (let i = 3; i < data.length; i += 4 * 64) {
    if (data[i] !== 0) {
      return false;
    }
  }
  return true;
}

/**
 * Poster frame (≤0.5s in) as a JPEG, plus duration; null on failure.
 *
 * Two WebKit traps (#1837, both reproduced in a WKWebView): a non-blob source (the
 * loopback media server) taints the canvas unless the element asks for CORS, so
 * `toBlob` throws; and on a blob source `seeked` fires before the frame can be
 * drawn, so the canvas comes out transparent. Hence `crossOrigin`, waiting for
 * the next presented frame where the webview can say, and refusing a blank draw.
 */
export function captureVideoPoster(src: string): Promise<CapturedPoster | null> {
  return new Promise((resolve) => {
    const vid = document.createElement('video');
    vid.muted = true;
    vid.playsInline = true;
    vid.preload = 'metadata';
    if (!src.startsWith('blob:') && !src.startsWith('data:')) {
      vid.crossOrigin = 'anonymous';
    }

    let settled = false;
    const finish = (result: CapturedPoster | null) => {
      if (settled) { return; }
      settled = true;
      vid.removeAttribute('src');
      vid.load();
      resolve(result);
    };

    let drawn = false;
    const draw = () => {
      if (settled || drawn) { return; }
      drawn = true;
      const canvas = document.createElement('canvas');
      // Cap to 1280px to stay well within WebKit/GDK's native surface limits.
      const MAX_DIM = 1280;
      let cw = vid.videoWidth;
      let ch = vid.videoHeight;
      if (!cw || !ch) { finish(null); return; }
      if (cw > MAX_DIM) { ch = Math.round(ch * MAX_DIM / cw); cw = MAX_DIM; }
      if (ch > MAX_DIM) { cw = Math.round(cw * MAX_DIM / ch); ch = MAX_DIM; }
      canvas.width = cw;
      canvas.height = ch;
      const ctx = canvas.getContext('2d', { willReadFrequently: true });
      if (!ctx) { finish(null); return; }
      try {
        ctx.drawImage(vid, 0, 0, cw, ch);
        if (drewNothing(ctx, cw, ch)) { finish(null); return; }
        canvas.toBlob((blob) => {
          finish(blob ? { url: URL.createObjectURL(blob), blob, duration } : null);
        }, 'image/jpeg', 0.8);
      } catch {
        // A tainted canvas (no CORS on the source) throws here.
        finish(null);
      }
    };

    let duration = 0;
    vid.addEventListener('loadedmetadata', () => {
      duration = isFinite(vid.duration) && vid.duration > 0 ? vid.duration : 0;
      vid.currentTime = Math.min(0.5, duration > 0 ? duration * 0.1 : 0.5);
    }, { once: true });

    vid.addEventListener('seeked', () => {
      if (typeof vid.requestVideoFrameCallback === 'function') {
        vid.requestVideoFrameCallback(() => draw());
        // A webview that never presents a paused frame still gets a try.
        setTimeout(draw, 1500);
      } else {
        draw();
      }
    }, { once: true });

    vid.addEventListener('error', () => { finish(null); }, { once: true });

    setTimeout(() => { finish(null); }, 8000);

    vid.src = src;
    vid.load();
  });
}

/** A Blob as standard base64 (no `data:` prefix), for a poster frame handed to the core. */
export function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = String(reader.result ?? '');
      resolve(result.slice(result.indexOf(',') + 1));
    };
    reader.onerror = () => reject(reader.error ?? new Error('read failed'));
    reader.readAsDataURL(blob);
  });
}
