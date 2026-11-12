import React from "react";
import { Card } from "../ui/Card";

/** The narrow card of the sign-in and PIN screens, vertically centred in a scrolling column above the backdrop. */
export const AuthCard: React.FC<{ children: React.ReactNode }> = ({ children }) => (
  <div
    className="flex-1 flex justify-center overflow-y-auto"
    style={{ position: "relative", zIndex: 1 }}
  >
    <Card padding="lg" className="my-auto" style={{ width: "100%", maxWidth: 360 }}>
      {children}
    </Card>
  </div>
);
