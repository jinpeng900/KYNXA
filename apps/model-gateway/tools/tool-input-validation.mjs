import { toolFailure } from '../platform/tool-paths.mjs';

/** Validate owned JSON contracts recursively before dispatch, including object arrays.
 * 派发前递归验证自有 JSON 合同，包含对象数组；第三方 schema 仍由 MCP 客户端验证。 */
export function validateBuiltinInput(descriptor, input) {
  const validate = (schema, value, path, depth = 0) => {
    if (depth > 16) throw toolFailure(`工具参数 ${path} 嵌套过深。`);
    const types = Array.isArray(schema.type) ? schema.type : [schema.type];
    const type = value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value;
    if (schema.type && !(types.includes(type) || types.includes('integer') && Number.isSafeInteger(value)))
      throw toolFailure(`工具参数 ${path} 类型无效。`);
    if (schema.enum && !schema.enum.includes(value)) throw toolFailure(`工具参数 ${path} 值无效。`);
    if (typeof value === 'string' && (value.includes('\0') || value.length < (schema.minLength ?? 0) ||
        value.length > (schema.maxLength ?? Infinity))) throw toolFailure(`工具参数 ${path} 长度或内容无效。`);
    if (typeof value === 'number' && (!Number.isFinite(value) || value < (schema.minimum ?? -Infinity) ||
        value > (schema.maximum ?? Infinity))) throw toolFailure(`工具参数 ${path} 超出范围。`);
    if (Array.isArray(value)) {
      if (value.length < (schema.minItems ?? 0) || value.length > (schema.maxItems ?? 64))
        throw toolFailure(`工具参数 ${path} 项数无效。`);
      if (schema.items) value.forEach((item, index) => validate(schema.items, item, `${path}[${index}]`, depth + 1));
    } else if (value && typeof value === 'object') {
      for (const key of schema.required ?? []) if (!Object.hasOwn(value, key))
        throw toolFailure(`缺少工具参数 ${path}.${key}。`);
      for (const [key, item] of Object.entries(value)) {
        const property = Object.hasOwn(schema.properties ?? {}, key) ? schema.properties[key] : undefined;
        if (!property && schema.additionalProperties === false) throw toolFailure(`不支持工具参数 ${path}.${key}。`);
        if (property) validate(property, item, `${path}.${key}`, depth + 1);
      }
    }
  };
  validate(descriptor.inputSchema, input, descriptor.name);
}
