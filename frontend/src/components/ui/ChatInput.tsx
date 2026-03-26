import React, { useState, useRef, useCallback, useEffect } from "react";
import { ChevronRight, Plus, X, Image as ImageIcon, File as FileIcon } from "lucide-react";

export interface Attachment {
  id: string;
  file: File;
  preview?: string;
  type: "image" | "file";
}

interface ChatInputProps {
  onSend: (message: string, attachments: Attachment[]) => void;
  placeholder?: string;
  disabled?: boolean;
  autoFocus?: boolean;
  className?: string;
  maxAttachments?: number;
  maxFileSize?: number;
}

const formatFileSize = (bytes: number): string => {
  if (bytes === 0) { return "0B"; }
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + sizes[i];
};

export const ChatInput: React.FC<ChatInputProps> = ({
  onSend,
  placeholder = "Type a message…",
  disabled = false,
  autoFocus = false,
  className = "",
  maxAttachments = 5,
  maxFileSize = 10 * 1024 * 1024,
}) => {
  const [message, setMessage] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [isDragOver, setIsDragOver] = useState(false);
  const [isFocused, setIsFocused] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const handleFileAdd = useCallback((file: File) => {
    if (attachments.length >= maxAttachments) { return; }
    if (file.size > maxFileSize) { return; }

    const attachment: Attachment = {
      id: `${Date.now()}-${Math.random()}`,
      file,
      type: file.type.startsWith("image/") ? "image" : "file",
    };

    if (attachment.type === "image") {
      const reader = new FileReader();
      reader.onload = (e) => {
        attachment.preview = e.target?.result as string;
        setAttachments((prev) => [...prev, attachment]);
      };
      reader.readAsDataURL(file);
    } else {
      setAttachments((prev) => [...prev, attachment]);
    }
  }, [attachments.length, maxAttachments, maxFileSize]);

  useEffect(() => {
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
      const maxH = 24 * 6;
      textareaRef.current.style.height = `${Math.min(textareaRef.current.scrollHeight, maxH)}px`;
    }
  }, [message]);

  const handleSend = () => {
    if (!message.trim() && attachments.length === 0) { return; }
    onSend(message.trim(), attachments);
    setMessage("");
    setAttachments([]);
    textareaRef.current?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handlePaste = useCallback((e: React.ClipboardEvent) => {
    const items = e.clipboardData.items;
    let hasFiles = false;
    for (let i = 0; i < items.length; i++) {
      if (items[i].type.startsWith("image/")) {
        const file = items[i].getAsFile();
        if (file) { handleFileAdd(file); hasFiles = true; }
      }
    }
    if (hasFiles) { e.preventDefault(); }
  }, [handleFileAdd]);

  return (
    <div
      className={`border-t ${className}`}
      style={{
        borderColor: "var(--c-border)",
        background: "var(--c-bg)",
      }}
      onDragOver={(e) => { e.preventDefault(); setIsDragOver(true); }}
      onDragLeave={() => setIsDragOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setIsDragOver(false);
        Array.from(e.dataTransfer.files).forEach(handleFileAdd);
      }}
    >
      {/* Attachment chips */}
      {attachments.length > 0 && (
        <div
          className="px-2 py-1 flex items-center gap-1.5 flex-wrap"
          style={{ borderBottom: "1px solid var(--c-border)" }}
        >
          {attachments.map((att) => {
            const Icon = att.type === "image" ? ImageIcon : FileIcon;
            return (
              <div
                key={att.id}
                className="inline-flex items-center gap-1.5 px-2 py-0.5 text-xs font-mono"
                style={{
                  background: "var(--c-hover)",
                  border: "1px solid var(--c-border)",
                  borderRadius: "4px",
                  color: "var(--c-text)",
                }}
              >
                <Icon className="w-3 h-3 flex-shrink-0" style={{ color: "var(--c-text-dim)" }} />
                <span className="truncate max-w-28">{att.file.name}</span>
                <span style={{ color: "var(--c-text-muted)" }}>{formatFileSize(att.file.size)}</span>
                <button
                  onClick={() => setAttachments((p) => p.filter((a) => a.id !== att.id))}
                  aria-label={`Remove ${att.file.name}`}
                  style={{ color: "var(--c-text-muted)" }}
                  className="transition-colors"
                  onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
                  onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
                >
                  <X className="w-3 h-3" />
                </button>
              </div>
            );
          })}
        </div>
      )}

      {/* Input row */}
      <div className="flex items-start gap-1 px-2 py-1.5">
        <button
          onClick={() => fileInputRef.current?.click()}
          disabled={disabled || attachments.length >= maxAttachments}
          aria-label="Add attachment"
          className="p-1.5 flex-shrink-0 transition-colors"
          style={{ color: "var(--c-text-muted)", opacity: disabled ? 0.4 : 1 }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
        >
          <Plus className="w-4 h-4" />
        </button>

        <textarea
          ref={textareaRef}
          data-testid="message-input"
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          onFocus={() => setIsFocused(true)}
          onBlur={() => setIsFocused(false)}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
          placeholder={placeholder}
          disabled={disabled}
          autoFocus={autoFocus}
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          rows={1}
          className="chat-input-textarea flex-1 min-w-0 px-2 py-1 resize-none font-mono text-sm transition-colors"
          style={{
            lineHeight: "1.5rem",
            minHeight: "1.5rem",
            borderRadius: "4px",
            background: isFocused ? "var(--c-accent)" : "var(--c-hover)",
            color: isFocused ? "var(--c-bg)" : "var(--c-text)",
            outline: "none",
            border: "none",
            opacity: disabled ? 0.5 : 1,
          }}
          aria-label="Message input"
        />

        <button
          onClick={handleSend}
          disabled={disabled || (!message.trim() && attachments.length === 0)}
          data-testid="message-send-button"
          aria-label="Send message"
          className="p-1.5 flex-shrink-0 transition-colors"
          style={{ color: "var(--c-text-muted)", opacity: disabled || (!message.trim() && !attachments.length) ? 0.3 : 1 }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
        >
          <ChevronRight className="w-4 h-4" />
        </button>
      </div>

      <input
        ref={fileInputRef}
        type="file"
        multiple
        onChange={(e) => {
          Array.from(e.target.files || []).forEach(handleFileAdd);
          e.target.value = "";
        }}
        className="hidden"
      />

      {isDragOver && (
        <div
          className="px-2 py-1 text-center text-xs font-mono"
          style={{ color: "var(--c-text-muted)", borderTop: "1px solid var(--c-border)" }}
        >
          drop files
        </div>
      )}
    </div>
  );
};
