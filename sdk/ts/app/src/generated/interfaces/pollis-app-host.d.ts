/// <reference path="./pollis-app-types.d.ts" />
declare module 'pollis:app/host@0.1.0' {
  /**
   * Queue a new message in a granted channel. Applied only if the handler
   * returns ok.
   */
  export function post(channel: ChannelId, text: string): void;
  /**
   * Queue a reply to a message in a granted channel.
   */
  export function reply(message: MessageId, text: string): void;
  /**
   * Queue a reaction on a message in a granted channel.
   */
  export function react(message: MessageId, emoji: string): void;
  /**
   * Queue an edit of a message this bot sent.
   */
  export function editOwn(message: MessageId, text: string): void;
  /**
   * Queue deletion of a message this bot sent.
   */
  export function deleteOwn(message: MessageId): void;
  /**
   * Messages from granted channels inside the granted history window.
   */
  export function queryMessages(filter: MessageFilter): Array<Message>;
  /**
   * Sealed per-installation key-value state, within the manifest's quota.
   * Writes become durable only if the handler returns ok.
   */
  export function kvGet(key: string): Uint8Array | undefined;
  export function kvSet(key: string, value: Uint8Array): void;
  export function kvDelete(key: string): void;
  /**
   * One HTTPS request to a host in the granted egress list. Redirects are
   * followed only within the same host.
   */
  export function httpFetch(request: HttpRequest): HttpResponse;
  /**
   * Write to the runtime's log. Never reaches the group.
   */
  export function log(level: LogLevel, text: string): void;
  /**
   * Unix milliseconds.
   */
  export function now(): bigint;
  /**
   * A cryptographically random value.
   */
  export function random(): bigint;
  export type MessageId = import('pollis:app/types@0.1.0').MessageId;
  export type ChannelId = import('pollis:app/types@0.1.0').ChannelId;
  export type Message = import('pollis:app/types@0.1.0').Message;
  export type MessageFilter = import('pollis:app/types@0.1.0').MessageFilter;
  export type HttpRequest = import('pollis:app/types@0.1.0').HttpRequest;
  export type HttpResponse = import('pollis:app/types@0.1.0').HttpResponse;
  export type LogLevel = import('pollis:app/types@0.1.0').LogLevel;
}
