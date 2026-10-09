import { StreamFailure } from './streaming.mjs';

// Slot identifiers do not allocate arrays; distinct calls and buffered arguments own the limits.
// 序号不用于分配数组；真正限制独立调用数量和累计参数缓冲，避免把序号当作执行预算。
export const MAX_TURN_TOOL_CALLS = 32;
export const MAX_TOOL_ARGUMENT_CHARACTERS = 65536;
export const MAX_TURN_ARGUMENT_CHARACTERS = 1024 * 1024;

/** Only decoder-owned failures certify that this model step dispatched no effects.
 * 仅解码器自身的错误可以证明当前模型步骤尚未派发副作用，不泛化为业务执行失败。 */
export class ToolCallDecodeFailure extends StreamFailure {
  constructor(message, code, type = 'error') {
    super(message, type);
    this.code = code;
    this.recoverable = true;
    this.executed = false;
  }
}

export function validateTurnCallCount(count) {
  if (!Number.isSafeInteger(count) || count < 0 || count > MAX_TURN_TOOL_CALLS)
    throw new ToolCallDecodeFailure('模型单次请求的工具数量超过上限。', 'MODEL_TOOL_CALL_LIMIT');
}

export function validateArgumentBuffer(argumentsText, aggregateCharacters = argumentsText.length) {
  if (argumentsText.length > MAX_TOOL_ARGUMENT_CHARACTERS || aggregateCharacters > MAX_TURN_ARGUMENT_CHARACTERS)
    throw new ToolCallDecodeFailure('工具参数超过大小限制。', 'MODEL_TOOL_ARGUMENT_LIMIT');
}
