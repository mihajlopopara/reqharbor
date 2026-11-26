import type { CommandError, CommandErrorCode } from "../types";

export class MockError implements CommandError {
  code: CommandErrorCode;
  message: string;
  details?: CommandError["details"];
  constructor(code: CommandErrorCode, message: string, details?: CommandError["details"]) {
    this.code = code;
    this.message = message;
    if (details !== undefined) this.details = details;
  }
}

export const fail = (code: CommandErrorCode, message: string, details?: CommandError["details"]): never => {
  throw new MockError(code, message, details);
};
