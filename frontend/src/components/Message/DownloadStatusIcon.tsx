import React from "react";
import { Download, Check } from "lucide-react";
import { LoadingSpinner } from "../ui/LoaderSpinner";

interface DownloadStatusIconProps {
  status: "idle" | "downloading" | "done";
}

/** An attachment download button's glyph: download, a spinner while saving, a check once saved. */
export const DownloadStatusIcon: React.FC<DownloadStatusIconProps> = ({ status }) => {
  if (status === "downloading") {
    return <LoadingSpinner size="sm" />;
  }
  if (status === "done") {
    return <Check size={14} aria-hidden="true" />;
  }
  return <Download size={14} aria-hidden="true" />;
};
