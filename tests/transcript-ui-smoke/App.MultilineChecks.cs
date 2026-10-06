using System.Globalization;
using System.Text.Json;
using KYNXA_Desktop.Services;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckMultilineRepliesAsync()
    {
        var originalSize = _window!.AppWindow.Size;
        string[] lines = Enumerable.Range(1, 20).Select(index => index.ToString("D3", CultureInfo.InvariantCulture)).ToArray();
        string selectedSource = string.Join("\n", lines);
        var measurements = new List<JsonElement>();
        _transcript.ClearSelection();
        try
        {
            await EvalAsync<bool>("""
                (() => {
                  window.__multilineSmoke = {
                    body(id) {
                      const body = document.querySelector(`article[data-message-id="${id}"] .message-body`);
                      if (!body) throw new Error('Missing multiline reply: ' + id);
                      return body;
                    },
                    range(id, token) {
                      const node = __transcriptSmoke.textNode(this.body(id), token);
                      const start = node.nodeValue.indexOf(token);
                      const range = document.createRange();
                      range.setStart(node, start); range.setEnd(node, start + token.length);
                      return range;
                    },
                    geometry(id, count) {
                      return {
                        viewport: innerWidth,
                        pageWidth: document.documentElement.scrollWidth,
                        glyphs: Array.from({ length: count }, (_, index) => {
                          const token = String(index + 1).padStart(3, '0');
                          const rect = this.range(id, token).getBoundingClientRect();
                          return { token, top: rect.top, bottom: rect.bottom, height: rect.height };
                        })
                      };
                    },
                    copy(id, count) {
                      const first = this.range(id, '001');
                      const last = this.range(id, String(count).padStart(3, '0'));
                      const selection = getSelection();
                      selection.removeAllRanges();
                      selection.setBaseAndExtent(first.startContainer, first.startOffset, last.endContainer, last.endOffset);
                      return __transcriptSmoke.copyEvent();
                    }
                  };
                  return true;
                })()
                """);

            foreach (string newline in new[] { "\n", "\r\n" })
            {
                string label = newline == "\n" ? "LF" : "CRLF";
                string source = string.Join(newline, lines);
                var chat = Guid.NewGuid();
                var reply = Message(chat, "assistant", source);
                var singleLine = Message(chat, "assistant", "001002");
                _transcript.ClearSelection();
                _transcript.ShowConversation(chat, [reply, singleLine]);
                await WaitAsync($"document.querySelector('article[data-message-id=\"{reply.Message.Id}\"] .message-body')?.textContent.includes('020') === true && document.querySelector('article[data-message-id=\"{singleLine.Message.Id}\"] .message-body')?.textContent.trim() === '001002'",
                    label + " completed multiline and single-line control render as real assistant replies");

                foreach (int width in new[] { 480, 980, 1600 })
                {
                    _transcript.ClearSelection();
                    _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 1000));
                    await Task.Delay(220);
                    await EvalAsync<bool>("(() => { scrollTo(0, 0); return true; })()");
                    var geometry = await ReadMultilineGeometryAsync(reply.Message.Id, 20);
                    measurements.Add(geometry);
                    _metrics["multilineReplies"] = measurements;
                    Check(Math.Abs(geometry.GetProperty("viewport").GetDouble() - width) < 30,
                        label + " multiline viewport reaches the requested native width " + width);
                    CheckMultilineGeometry(geometry, 20, label + " completed reply at " + width);
                    Check(geometry.GetProperty("pageWidth").GetDouble() <= geometry.GetProperty("viewport").GetDouble() + 1,
                        label + " multiline reply does not widen the page at " + width);

                    var control = await ReadMultilineGeometryAsync(singleLine.Message.Id, 2);
                    var controlGlyphs = control.GetProperty("glyphs").EnumerateArray().ToArray();
                    Check(controlGlyphs.All(glyph => glyph.GetProperty("height").GetDouble() > 0) &&
                        Math.Abs(controlGlyphs[0].GetProperty("top").GetDouble() - controlGlyphs[1].GetProperty("top").GetDouble()) < 1,
                        "001002 stays on one glyph baseline without guessed numeric line breaks at " + width);
                    string selected = await EvalAsync<string>($"__multilineSmoke.copy('{reply.Message.Id}', 20)");
                    Check(selected == selectedSource,
                        label + " selected multiline copy preserves every LF separator without added spaces at " + width);
                    _transcript.ClearSelection();

                    if (label == "CRLF" && width == 980)
                    {
                        string path = Path.Combine(Path.GetTempPath(), "kynxa-transcript-multiline.png");
                        await CaptureViewportAsync(path);
                        _metrics["multilinePreview"] = path;
                    }
                }

                await CheckMultilineWholeCopyAsync(reply.Message.Id, source, label + " completed reply");
                Check(reply.Message.Content == source, label + " rendering and copy leave completed source unchanged");

                // Split CRLF between chunks so a live carriage return cannot lose the next line.
                // 在流片段之间拆开 CRLF，验证到达中的回车不会吞掉后续换行。
                string[] chunks =
                [
                    string.Join(newline, lines.Take(5)) + (newline == "\r\n" ? "\r" : "\n"),
                    (newline == "\r\n" ? "\n" : "") + string.Join(newline, lines.Skip(5).Take(8)) + (newline == "\r\n" ? "\r" : "\n"),
                    (newline == "\r\n" ? "\n" : "") + string.Join(newline, lines.Skip(13))
                ];
                int[] counts = [5, 13, 20];
                var streamingChat = Guid.NewGuid();
                var streamingReply = Message(streamingChat, "assistant", chunks[0], "streaming");
                _transcript.ClearSelection();
                _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 1000));
                await Task.Delay(220);
                _transcript.ShowConversation(streamingChat, [streamingReply]);
                for (int phase = 0; phase < chunks.Length; phase++)
                {
                    if (phase > 0)
                    {
                        streamingReply.Message.Content += chunks[phase];
                        streamingReply.Refresh();
                    }
                    string last = lines[counts[phase] - 1];
                    string expectedStage = string.Concat(lines.Take(counts[phase]));
                    _metrics["multilineStage"] = label + " streaming chunk through " + last;
                    await WaitAsync($"document.querySelector('article[data-message-id=\"{streamingReply.Message.Id}\"] .message-body')?.textContent.replace(/\\s/g, '') === '{expectedStage}'",
                        label + " streaming chunk renders its complete exact sequence through " + last);
                    CheckMultilineGeometry(await ReadMultilineGeometryAsync(streamingReply.Message.Id, counts[phase]), counts[phase],
                        label + " streaming chunk through " + last);
                }

                streamingReply.Message.Status = "completed";
                streamingReply.Refresh();
                await WaitAsync($"document.querySelector('article[data-message-id=\"{streamingReply.Message.Id}\"]')?.dataset.presentationMode === 'final' && document.querySelector('article[data-message-id=\"{streamingReply.Message.Id}\"] .message-body')?.textContent.includes('020') === true",
                    label + " stream finalization keeps the complete reply");
                CheckMultilineGeometry(await ReadMultilineGeometryAsync(streamingReply.Message.Id, 20), 20, label + " finalized stream");
                Check(await EvalAsync<string>($"__multilineSmoke.copy('{streamingReply.Message.Id}', 20)") == selectedSource,
                    label + " finalized stream selection copies all lines without added spaces");
                _transcript.ClearSelection();
                await CheckMultilineWholeCopyAsync(streamingReply.Message.Id, source, label + " finalized stream");
                Check(streamingReply.Message.Content == source, label + " stream assembly and final rendering preserve original separators");
            }
        }
        finally
        {
            _transcript.ClearSelection();
            await EvalAsync<bool>("(() => { delete window.__multilineSmoke; return true; })()");
            _window.AppWindow.Resize(originalSize);
            await Task.Delay(150);
        }
    }

    private async Task<JsonElement> ReadMultilineGeometryAsync(Guid messageId, int lineCount)
    {
        // WebView reports an uncaught script error as null, which would hide the missing token.
        // WebView 把未捕获脚本异常返回为 null，因此保留具体节点错误便于定位。
        var geometry = await EvalAsync<JsonElement>($$"""
            (() => {
              try { return __multilineSmoke.geometry('{{messageId}}', {{lineCount}}); }
              catch (error) {
                const body = document.querySelector('article[data-message-id="{{messageId}}"] .message-body');
                return {
                  error: String(error?.stack || error),
                  bodyText: body?.textContent || ''
                };
              }
            })()
            """);
        if (geometry.ValueKind != JsonValueKind.Object)
        {
            throw new InvalidOperationException($"Multiline geometry for {messageId}, {lineCount} lines returned {geometry.ValueKind} instead of a measurement object.");
        }
        if (geometry.TryGetProperty("error", out var error))
        {
            throw new InvalidOperationException($"Multiline geometry for {messageId}, {lineCount} lines failed: {error.GetString()}; text={geometry.GetProperty("bodyText").GetString()}");
        }
        return geometry;
    }

    private void CheckMultilineGeometry(JsonElement geometry, int lineCount, string description)
    {
        var glyphs = geometry.GetProperty("glyphs").EnumerateArray().ToArray();
        Check(glyphs.Length == lineCount && glyphs.All(glyph => glyph.GetProperty("height").GetDouble() > 0),
            description + " has a real visible glyph rectangle for every source line");
        for (int index = 1; index < glyphs.Length; index++)
        {
            Check(glyphs[index].GetProperty("top").GetDouble() > glyphs[index - 1].GetProperty("top").GetDouble() + 1,
                description + " places " + glyphs[index].GetProperty("token").GetString() + " below the preceding line");
        }
    }

    private async Task CheckMultilineWholeCopyAsync(Guid messageId, string source, string description)
    {
        var feedback = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        EventHandler<string> callback = (_, text) => feedback.TrySetResult(text);
        _transcript.ActionFeedbackRequested += callback;
        try
        {
            await EvalAsync<bool>($"(() => {{ document.querySelector('article[data-message-id=\"{messageId}\"] .copy-message').click(); return true; }})()");
            Check(await feedback.Task.WaitAsync(TimeSpan.FromSeconds(5)) == UiText.Get("已复制"),
                description + " whole-message copy receives the real native acknowledgement");
            Check(await Clipboard.GetContent().GetTextAsync() == source,
                description + " whole-message clipboard retains exact original LF or CRLF source");
        }
        finally
        {
            _transcript.ActionFeedbackRequested -= callback;
        }
    }
}
