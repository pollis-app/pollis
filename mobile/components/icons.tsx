// Thin wrapper over lucide-react-native so screens use a stable `Icon.*` API.
// The design spec commits to a 1.2px monoline stroke — lucide's default is 2,
// so we pin strokeWidth here. Verified-peer "notch" is a rotated <View> in
// ui.tsx (Diamond), not an icon.
import {
  Hash,
  Search,
  Settings,
  Plus,
  SendHorizontal,
  AtSign,
  Users,
  Bell,
  Inbox,
  Pencil,
  MoreVertical,
  Lock,
  Shield,
  Mic,
  MicOff,
  Headphones,
  User,
  LogOut,
  Check,
  Mail,
  Key,
  Smartphone,
  ChevronLeft,
  ChevronRight,
  ArrowLeft,
  ArrowRight,
  Diamond,
  Volume2,
  type LucideIcon,
} from "lucide-react-native";
import { semantic } from "../theme/tokens";

type P = { size?: number; color?: string };

const wrap =
  (C: LucideIcon, defaultSize = 14) =>
  ({ size, color }: P) => (
    <C
      size={size ?? defaultSize}
      color={color ?? semantic.ink}
      strokeWidth={1.2}
    />
  );

export const Icon = {
  back: wrap(ChevronLeft),
  fwd: wrap(ChevronRight),
  arrowLeft: wrap(ArrowLeft),
  arrowRight: wrap(ArrowRight),
  search: wrap(Search, 16),
  gear: wrap(Settings, 16),
  plus: wrap(Plus),
  send: wrap(SendHorizontal, 16),
  hash: wrap(Hash),
  speak: wrap(Volume2),
  at: wrap(AtSign),
  people: wrap(Users),
  bell: wrap(Bell),
  inbox: wrap(Inbox),
  edit: wrap(Pencil),
  kebab: wrap(MoreVertical),
  lock: wrap(Lock, 12),
  shield: wrap(Shield),
  mic: wrap(Mic),
  micOff: wrap(MicOff),
  headphones: wrap(Headphones),
  user: wrap(User),
  exit: wrap(LogOut),
  check: wrap(Check, 12),
  mail: wrap(Mail),
  key: wrap(Key),
  device: wrap(Smartphone),
  diamond: wrap(Diamond, 16),
};

export type IconName = keyof typeof Icon;
