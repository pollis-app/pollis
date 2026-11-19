/// <reference path="./interfaces/pollis-app-host.d.ts" />
/// <reference path="./interfaces/pollis-app-types.d.ts" />
declare module 'pollis:app/app@0.1.0' {
  export type Message = import('pollis:app/types@0.1.0').Message;
  export type Command = import('pollis:app/types@0.1.0').Command;
  export type Schedule = import('pollis:app/types@0.1.0').Schedule;
  export type Request = import('pollis:app/types@0.1.0').Request;
  export type Webhook = import('pollis:app/types@0.1.0').Webhook;
  export type * as PollisAppHost010 from 'pollis:app/host@0.1.0'; // import pollis:app/host@0.1.0
  export type * as PollisAppTypes010 from 'pollis:app/types@0.1.0'; // import pollis:app/types@0.1.0
  export function onMessage(message: Message): void;
  export function onMention(message: Message): void;
  export function onCommand(command: Command): void;
  export function onSchedule(schedule: Schedule): void;
  export function onRequest(request: Request): void;
  export function onWebhook(webhook: Webhook): void;
  export type Result<T, E> = { tag: 'ok', val: T } | { tag: 'err', val: E };
}
