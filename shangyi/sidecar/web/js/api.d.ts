export declare const api: {
    chats: () => Promise<any>;
    chat: (id: any) => Promise<any>;
    startChat: (body: any) => Promise<any>;
    chatMessage: (id: any, body: any) => Promise<any>;
    stopChat: (id: any) => Promise<any>;
    health: () => Promise<any>;
    cases: () => Promise<any>;
    case: (date: any, id: any) => Promise<any>;
    library: () => Promise<any>;
    playbooks: () => Promise<any>;
    solve: (payload: any) => Promise<any>;
    run: (payload: any) => Promise<any>;
    runs: () => Promise<any>;
    stopRun: (id: any) => Promise<any>;
};
export declare function startPoll(fn: any, ms?: number): number;
export declare function stopPoll(t: any): void;
export declare function stopAllPolls(): void;
