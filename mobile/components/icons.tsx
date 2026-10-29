// Stable `Icon.*` wrapper over lucide-react-native at a 1.75pt stroke. Decorative to screen readers.
import {
  Hash,
  Search,
  Settings,
  Plus,
  SendHorizontal,
  AtSign,
  Users,
  UserPlus,
  Bell,
  BellOff,
  Inbox,
  Pencil,
  MoreVertical,
  Ellipsis,
  Lock,
  LockKeyhole,
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
  Pin,
  PinOff,
  CheckCheck,
  ChevronLeft,
  ChevronRight,
  ChevronDown,
  Copy,
  Link2,
  Share2,
  Download,
  ExternalLink,
  AlertCircle,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  Volume2,
  Info,
  MessagesSquare,
  MessageCircle,
  Flag,
  Globe,
  Reply,
  Smile,
  SmilePlus,
  Trash2,
  X,
  SlidersHorizontal,
  Contrast,
  Paperclip,
  Delete,
  UserX,
  Ban,
  Play,
  Pause,
  Film,
  Eye,
  EyeOff,
  File,
  ImageDown,
  RotateCcw,
  RotateCw,
  Stethoscope,
  Vault,
  Images,
  MessageSquare,
  type LucideIcon,
} from "lucide-react-native";
import { I18nManager, View } from "react-native";
import { semantic } from "../theme/tokens";

export const ICON_STROKE = 1.75;

type P = { size?: number; color?: string };

const wrap =
  (C: LucideIcon, defaultSize = 20) =>
  ({ size, color }: P) => (
    <C
      size={size ?? defaultSize}
      color={color ?? semantic.text}
      strokeWidth={ICON_STROKE}
    />
  );

// Only icons that ENCODE direction are mirrored under RTL (#1074).
const mirrored =
  (C: LucideIcon, defaultSize = 20) =>
  ({ size, color }: P) => (
    <View style={I18nManager.isRTL ? { transform: [{ scaleX: -1 }] } : undefined}>
      <C
        size={size ?? defaultSize}
        color={color ?? semantic.text}
        strokeWidth={ICON_STROKE}
      />
    </View>
  );

// Pre-redesign keys keep their old default sizes; new keys default to 20.
export const Icon = {
  /* ── Navigation ── */
  back: mirrored(ChevronLeft, 14),
  fwd: mirrored(ChevronRight, 14),
  chevronLeft: mirrored(ChevronLeft, 24),
  chevronRight: mirrored(ChevronRight, 18),
  chevronDown: wrap(ChevronDown, 18),
  arrowLeft: mirrored(ArrowLeft, 14),
  arrowRight: mirrored(ArrowRight, 14),
  arrowUp: wrap(ArrowUp),
  close: wrap(X),
  x: wrap(X),

  /* ── Tabs ── */
  users: wrap(Users, 24),
  messageCircle: wrap(MessageCircle, 24),
  search: wrap(Search, 16),
  user: wrap(User, 14),

  /* ── Actions ── */
  plus: wrap(Plus, 14),
  send: wrap(SendHorizontal, 16),
  reply: mirrored(Reply),
  smile: wrap(Smile),
  smilePlus: wrap(SmilePlus),
  pencil: wrap(Pencil),
  edit: wrap(Pencil, 14),
  copy: wrap(Copy, 14),
  link: wrap(Link2, 14),
  share: wrap(Share2, 14),
  download: wrap(Download, 14),
  external: wrap(ExternalLink, 16),
  trash: wrap(Trash2),
  logOut: wrap(LogOut),
  exit: wrap(LogOut, 14),
  userPlus: wrap(UserPlus),
  attach: wrap(Paperclip),
  more: wrap(Ellipsis),
  kebab: wrap(MoreVertical, 14),
  // Points at the text it erases, so it mirrors under RTL.
  backspace: mirrored(Delete, 26),
  userX: wrap(UserX),
  ban: wrap(Ban),

  /* ── Objects ── */
  hash: wrap(Hash, 14),
  at: wrap(AtSign, 14),
  people: wrap(Users, 14),
  bell: wrap(Bell, 14),
  bellOff: wrap(BellOff, 14),
  globe: wrap(Globe),
  bookmark: wrap(Bookmark, 14),
  pin: wrap(Pin),
  pinOff: wrap(PinOff),
  shield: wrap(Shield, 14),
  lock: wrap(Lock, 12),
  lockKeyhole: wrap(LockKeyhole),
  mail: wrap(Mail, 14),
  sliders: wrap(SlidersHorizontal),
  appearance: wrap(Contrast),
  gear: wrap(Settings, 16),
  inbox: wrap(Inbox, 14),
  speak: wrap(Volume2, 14),
  mic: wrap(Mic, 14),
  micOff: wrap(MicOff, 14),
  headphones: wrap(Headphones, 14),
  key: wrap(Key, 14),
  device: wrap(Smartphone, 14),
  thread: wrap(MessagesSquare, 14),
  flag: wrap(Flag, 14),
  info: wrap(Info, 16),
  alert: wrap(AlertCircle, 14),
  diagnostics: wrap(Stethoscope, 14),
  vault: wrap(Vault),
  images: wrap(Images),
  messageSquare: wrap(MessageSquare),

  /* ── Media (#1248) ── */
  play: wrap(Play),
  pause: wrap(Pause),
  video: wrap(Film),
  eye: wrap(Eye, 16),
  eyeOff: wrap(EyeOff, 16),
  file: wrap(File),
  saveImage: wrap(ImageDown),
  // The arrows encode time, not reading direction: not mirrored.
  seekBack: wrap(RotateCcw),
  seekForward: wrap(RotateCw),

  /* ── State ── */
  check: wrap(Check, 12),
  checkCheck: wrap(CheckCheck, 12),
};

export type IconName = keyof typeof Icon;

// So a control can tell an icon element it may recolour from a composed node.
const ICON_COMPONENTS: ReadonlySet<unknown> = new Set<unknown>(Object.values(Icon));

export function isIconElement(node: unknown): boolean {
  return (
    typeof node === "object" &&
    node !== null &&
    "type" in node &&
    ICON_COMPONENTS.has((node as { type: unknown }).type)
  );
}
