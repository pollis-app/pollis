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

/** Poster frame (≤0.5s in) as a JPEG object URL the caller must revoke, plus duration; null on failure. */
export function captureVideoPoster(
  src: string,
): Promise<{ url: string; duration: number } | null> {
  return new Promise((resolve) => {
    const vid = document.createElement('video');
    vid.muted = true;
    vid.playsInline = true;
    vid.preload = 'metadata';

    let settled = false;
    const finish = (result: { url: string; duration: number } | null) => {
      if (settled) { return; }
      settled = true;
      vid.src = '';
      vid.load();
      resolve(result);
    };

    let duration = 0;
    vid.addEventListener('loadedmetadata', () => {
      duration = isFinite(vid.duration) && vid.duration > 0 ? vid.duration : 0;
      vid.currentTime = Math.min(0.5, duration > 0 ? duration * 0.1 : 0.5);
    }, { once: true });

    vid.addEventListener('seeked', () => {
      const canvas = document.createElement('canvas');
      // Cap to 1280px to stay well within WebKit/GDK's native surface limits.
      const MAX_DIM = 1280;
      let cw = vid.videoWidth || 320;
      let ch = vid.videoHeight || 180;
      if (cw > MAX_DIM) { ch = Math.round(ch * MAX_DIM / cw); cw = MAX_DIM; }
      if (ch > MAX_DIM) { cw = Math.round(cw * MAX_DIM / ch); ch = MAX_DIM; }
      canvas.width = cw;
      canvas.height = ch;
      const ctx = canvas.getContext('2d');
      if (!ctx) { finish(null); return; }
      ctx.drawImage(vid, 0, 0, cw, ch);
      canvas.toBlob((blob) => {
        finish(blob ? { url: URL.createObjectURL(blob), duration } : null);
      }, 'image/jpeg', 0.75);
    }, { once: true });

    vid.addEventListener('error', () => { finish(null); }, { once: true });

    setTimeout(() => { finish(null); }, 5000);

    vid.src = src;
    vid.load();
  });
}

/**
 * Validate if a file is an image and within size limits
 *
 * @param file - File to validate
 * @param maxSizeMB - Maximum file size in megabytes (default: 10)
 * @returns Error message if invalid, null if valid
 */
export function validateImageFile(
  file: File,
  maxSizeMB: number = 10
): string | null {
  if (!file.type.startsWith('image/')) {
    return 'File must be an image';
  }

  const maxSizeBytes = maxSizeMB * 1024 * 1024;
  if (file.size > maxSizeBytes) {
    return `File size must be less than ${maxSizeMB}MB`;
  }

  return null;
}

/**
 * Generate a thumbnail from an image file
 *
 * @param file - Original image file
 * @param size - Thumbnail size (square, default: 256)
 * @returns Thumbnail as a File object
 */
export async function generateThumbnail(
  file: File,
  size: number = 256
): Promise<File> {
  return resizeImage(file, {
    maxSize: size,
    quality: 0.8,
    outputFormat: 'image/jpeg',
  });
}
