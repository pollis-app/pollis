/**
 * Image processing utilities
 * Resize, compress, and optimize images for upload
 */

export interface ResizeOptions {
  maxSize?: number; // Max width/height in pixels (default: 1024)
  quality?: number; // JPEG quality 0-1 (default: 0.85)
  outputFormat?: 'image/jpeg' | 'image/png' | 'image/webp'; // Default: 'image/jpeg'
}

/**
 * Resize and optimize an image file for upload
 * Maintains aspect ratio and converts to specified format
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

      // Resize if larger than maxSize
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

      // Convert to blob with quality optimization
      canvas.toBlob(
        (blob) => {
          if (!blob) {
            reject(new Error('Failed to compress image'));
            return;
          }

          // Determine file extension based on output format
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

/**
 * Generate a blurhash string and original dimensions from any image URL (including
 * video poster blob URLs). Scales down to a tiny canvas before encoding so it's fast
 * regardless of the source image size.
 *
 * Returns null on any failure (missing canvas support, load error, encode error).
 */
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
      // 32-pixel wide canvas is enough resolution for blurhash.
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
  // Check if file is an image
  if (!file.type.startsWith('image/')) {
    return 'File must be an image';
  }

  // Check file size
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
