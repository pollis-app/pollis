declare module 'pollis:app/types@0.1.0' {
  /**
   * Opaque message id (the envelope id).
   */
  export type MessageId = string;
  /**
   * Opaque channel id.
   */
  export type ChannelId = string;
  export interface Author {
    id: string,
    displayName: string,
    isBot: boolean,
  }
  /**
   * Attachment metadata only. Attachment bytes are a later capability.
   */
  export interface Attachment {
    name: string,
    mimeType: string,
    size: bigint,
  }
  export interface Message {
    id: MessageId,
    channel: ChannelId,
    author: Author,
    text: string,
    attachments: Array<Attachment>,
    /**
     * The message this one replies to, if any.
     */
    replyTo?: MessageId,
    /**
     * The thread root this message belongs to, if any.
     */
    thread?: MessageId,
    /**
     * Unix milliseconds.
     */
    sentAt: bigint,
    /**
     * Unix milliseconds of the last edit, if edited.
     */
    editedAt?: bigint,
  }
  /**
   * A slash command addressed to this App (#2018).
   */
  export interface Command {
    /**
     * Command name without the leading slash.
     */
    name: string,
    /**
     * Everything after the command name, unparsed.
     */
    args: string,
    /**
     * The message that carried the command.
     */
    message: Message,
  }
  /**
   * A schedule firing.
   */
  export interface Schedule {
    /**
     * The schedule's name as declared by the App.
     */
    name: string,
    /**
     * The slot this firing is for, unix milliseconds.
     */
    slot: bigint,
  }
  /**
   * A request addressed to this App by a member.
   */
  export interface Request {
    id: string,
    requester: Author,
    body: string,
  }
  /**
   * An inbound webhook delivery.
   */
  export interface Webhook {
    deliveryId: string,
    headers: Array<[string, string]>,
    body: Uint8Array,
  }
  export interface MessageFilter {
    /**
     * Only this channel; all granted channels when absent.
     */
    channel?: ChannelId,
    /**
     * Unix milliseconds, inclusive. Clamped to the granted history window.
     */
    since?: bigint,
    /**
     * Unix milliseconds, exclusive.
     */
    until?: bigint,
    /**
     * Upper bound on results; the runtime may return fewer.
     */
    limit: number,
  }
  export interface HttpRequest {
    method: string,
    /**
     * Absolute `https://` URL whose host is in the granted egress list.
     */
    url: string,
    headers: Array<[string, string]>,
    body?: Uint8Array,
  }
  export interface HttpResponse {
    status: number,
    headers: Array<[string, string]>,
    body: Uint8Array,
  }
  /**
   * # Variants
   * 
   * ## `"trace"`
   * 
   * ## `"debug"`
   * 
   * ## `"info"`
   * 
   * ## `"warn"`
   * 
   * ## `"error"`
   */
  export type LogLevel = "trace" | "debug" | "info" | "warn" | "error";
}
