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
  Bookmark,
  CheckCheck,
  ChevronLeft,
  ChevronRight,
  Copy,
  Link2,
  Share2,
  AlertCircle,
  ArrowLeft,
  ArrowRight,
  Diamond,
  Volume2,
  Info,
  MessagesSquare,
  type LucideIcon,
} from "lucide-react-native";
import { I18nManager, View } from "react-native";
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

// Yoga mirrors layout under RTL but never the glyph inside an icon, so an
// icon that ENCODES direction ("back", "forward") is flipped here — and only
// those. A magnifier or a bell means the same thing in both directions, and a
// mirrored one would just be wrong (#1074, the same rule as desktop's
// `.rtl-mirror`).
const mirrored =
  (C: LucideIcon, defaultSize = 14) =>
  ({ size, color }: P) => (
    <View style={I18nManager.isRTL ? { transform: [{ scaleX: -1 }] } : undefined}>
      <C
        size={size ?? defaultSize}
        color={color ?? semantic.ink}
        strokeWidth={1.2}
      />
    </View>
  );

export const Icon = {
  back: mirrored(ChevronLeft),
  fwd: mirrored(ChevronRight),
  arrowLeft: mirrored(ArrowLeft),
  arrowRight: mirrored(ArrowRight),
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
  checkCheck: wrap(CheckCheck, 12),
  mail: wrap(Mail),
  key: wrap(Key),
  device: wrap(Smartphone),
  copy: wrap(Copy, 14),
  link: wrap(Link2, 14),
  share: wrap(Share2),
  alert: wrap(AlertCircle),
  diamond: wrap(Diamond, 16),
  info: wrap(Info, 16),
  thread: wrap(MessagesSquare, 14),
  bookmark: wrap(Bookmark, 14),
};

export type IconName = keyof typeof Icon;
