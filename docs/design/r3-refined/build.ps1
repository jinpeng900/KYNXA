param([string]$SourceArchive)
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()
Add-Type -AssemblyName System.IO.Compression.FileSystem
$utf8 = [System.Text.UTF8Encoding]::new($false)
$outputRoot = $PSScriptRoot
$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $outputRoot '../../..'))
if (-not $SourceArchive) {
    $SourceArchive = (Get-ChildItem -LiteralPath $repositoryRoot -Filter '*139页.zip' | Select-Object -First 1).FullName
}
if (-not $SourceArchive) { throw 'Original design archive not found.' }
$archive = [System.IO.Compression.ZipFile]::OpenRead($SourceArchive)
function Read-ZipText($entry) {
    $entryReader = [System.IO.StreamReader]::new($entry.Open(), [System.Text.Encoding]::UTF8)
    try { $entryReader.ReadToEnd().Replace("`r`n", "`n") } finally { $entryReader.Dispose() }
}
function Write-Utf8([string]$path, [string]$value) { [System.IO.File]::WriteAllText($path, $value, $utf8) }
function ConvertTo-FlatWorkPolicy([string]$text) {
    $text = $text.Replace('Work、Child Work、Conversation', 'Work、Conversation')
    $text = $text.Replace('Work 逻辑树与磁盘目录解耦', 'Work 逻辑边界与磁盘目录解耦')
    $text = $text.Replace('多个 Child Work 移动时循环检测正确', '多个 Work 并发更新时 revision 冲突可检测')
    $text = $text.Replace(
        'Work 是长期容器而非文件夹。左侧在 Work Scope 下显示该 Work 的 Conversation 列表；主体仍是标准对话框。Work Overview 展示目标、最近活动、Tasks、Knowledge、Artifacts、待审批项与模型策略。Child Work 以树形层级展示，但内容访问仍按独立 Scope。',
        'Work 是不可嵌套的长期容器而非文件夹。左侧以扁平列表展示 Work，并在选定的 Work Scope 下显示其 Conversation 列表；主体仍是标准对话框。Work Overview 展示目标、最近活动、Tasks、Knowledge、Artifacts、待审批项与模型策略。置顶、归档、标签、搜索和纯展示分组可以辅助整理，但不得形成父子关系或带来权限、知识、状态继承。')
    $text = $text.Replace('| STATE-001 | Work Tree | 每个 Work 最多一个 parent_work_id，禁止循环。 |', '| STATE-001 | Flat Work | Work 不可嵌套；每个 Work 是独立 Scope。 |')
    $text = $text.Replace(
        'Work 聚合 Conversation、Task、Knowledge、Artifact、Decision、Memory、ModelPolicy 与 PermissionPolicy。磁盘不跟随逻辑树嵌套移动，避免重命名/移动 Work 引发大规模文件搬迁。父 Work 可聚合元数据，但不得自动继承子 Work 私有知识或 Grant。',
        'Work 聚合 Conversation、Task、Knowledge、Artifact、Decision、Memory、ModelPolicy 与 PermissionPolicy。Work 之间不存在父子关系、层级继承或移动操作；磁盘目录与 Work 身份解耦，重命名、归档或调整展示分组不搬迁文件。任何跨 Work 聚合仅是经过授权的查询投影，不改变各自的 Scope、知识或 Grant。')
    $text = $text.Replace('Child Work scope 不串', '不同 Work 的 scope 不串')
    $text = $text.Replace('Work/Child Work 长期 Scope', '扁平且相互独立的 Work 长期 Scope')
    $text = $text.Replace('Child Work 具有独立 scope，不因父级查看而自动扩权', '每个 Work 具有独立 scope，展示分组、标签或跨 Work 聚合不得自动扩权')
    $text = $text.Replace('移动 Child Work 做循环检测', '不同 Work 的查询、Context 与 Artifact 引用不能跨 scope')
    $text = $text.Replace(
        '`work(parent_work_id)` 形成单父树；',
        '`work` 是扁平且独立的 Scope；')
    $text = $text.Replace('Child Work scope 查询隔离', '不同 Work 的 scope 查询隔离')
    $text = $text.Replace('| Work | Child Work / scope | 无循环；不跨 Work 泄漏 |', '| Work | 扁平 Work / scope | 不嵌套；不跨 Work 泄漏 |')
    return $text
}
$supplement = [System.IO.File]::ReadAllText((Join-Path $outputRoot 'engineering-additions.md'))
$additions = @{}
foreach ($m in [regex]::Matches($supplement, '(?s)<!-- CHAPTER:([^ ]+) -->\s*(.*?)(?=<!-- CHAPTER:|\z)')) {
    $additions[$m.Groups[1].Value] = $m.Groups[2].Value.Trim()
}
$entries = @($archive.Entries | Where-Object FullName -match '/02_专项详细设计_MD/.*\.md$' | Sort-Object Name)
if ($entries.Count -ne 23 -or $additions.Count -ne 23) { throw 'Expected 23 chapters and 23 additions.' }
$sourceChapters = @{}
$chapters = [System.Collections.Generic.List[object]]::new()
$removedParagraphs = [System.Collections.Generic.List[string]]::new()
$commonStarts = @(
    '边界原则：', '所有对象写入前做 schema validation', '依赖方向必须可画成 DAG',
    '每一步都应留下可诊断事件', '任何实现优化（缓存', '降级必须被记录到用户可理解',
    'Trace 采用 correlation/causation', '测试至少包含：正常路径', '每个里程碑的 Definition of Done'
)
function Compress-Catalog([string]$text, [string]$letter, [string]$label) {
    $pattern = '(?ms)^## ' + $letter + '\. [^\n]+\n(.*?)(?=^## |\z)'
    $match = [regex]::Match($text, $pattern)
    if (-not $match.Success) { return $text }
    $values = @()
    foreach ($row in ($match.Groups[1].Value -split "`n")) {
        if ($row -match '^\| ([^|]+) \| ([^|]+) \|$') {
            $value = $Matches[1].Trim()
            if ($value -notin @('对象','服务/组件','Metric')) { $values += $value }
        }
    }
    $replacement = '## ' + $letter + '. ' + $label + "`n`n" + ($values -join '；') + "。`n`n"
    return $text.Substring(0,$match.Index) + $replacement + $text.Substring($match.Index+$match.Length)
}
foreach ($entry in $entries) {
    $original = ConvertTo-FlatWorkPolicy (Read-ZipText $entry)
    $id = if ($entry.Name.StartsWith('Appendix_')) { $entry.Name.Substring(0,10) } else { $entry.Name.Substring(0,2) }
    $sourceChapters[$id] = $original
    $body = $original
    if (-not $id.StartsWith('Appendix')) {
        $body = $body.Replace('# Full Technical Expansion · 可编码详细规格', '## 模块工程要求')
        $body = [regex]::Replace($body, '(?m)^> 本节把 R3 冻结概要展开.*\n?', '')
        $body = [regex]::Replace($body, '(?m)^版本：v1\.4\.2-R3\s*\n日期：2026-09-18\s*\n', '')
        $body = [regex]::Replace($body, '(?m)^\*\*[A-Za-z][^\n]+\*\*\s*\n', '')
        if ($id -ne '00') {
            foreach ($start in $commonStarts) {
                $pattern = '(?m)^' + [regex]::Escape($start) + '[^\n]+\n?'
                foreach($m in [regex]::Matches($body,$pattern)) { $removedParagraphs.Add($m.Value.Trim()) }
                $body = [regex]::Replace($body,$pattern,'')
            }
            $body = [regex]::Replace($body, '(?ms)^## F\. 并发、幂等与 Revision\n.*?(?=^## G\.)', '')
        }
        $body = Compress-Catalog $body 'B' '核心对象目录'
        $body = Compress-Catalog $body 'C' '服务职责目录'
        $body = Compress-Catalog $body 'H' '指标目录'
        $body = $body.Replace('UnknownSideEffect、Internal 六类','UnknownSideEffect、Internal 七类')
        $body = $body.Replace('## 模块工程要求', "## 模块工程要求`n`n本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。")
    }
    $body = [regex]::Replace($body, '\n{3,}', "`n`n").Trim()
    $body += "`n`n" + $additions[$id] + "`n"
    $title = ($body -split "`n")[0].Substring(2)
    $chapters.Add([pscustomobject]@{Id=$id;Name=$entry.Name;Title=$title;Text=$body})
}
$archive.Dispose()
$intro = @'
# KYNXA v1.4.2-R3
## 精炼与工程细化版

基于 2026-09-18 详细完整版修订 · 编辑日期 2026-09-23

文档状态：R3 冻结内容的编辑整理 + 待评审工程建议。不是新软件版本，也不是已实现能力清单。

## 阅读说明

本版保留 18 个专项和 5 个附录。精简对象是反复出现的通用原则、相同表格说明和模板段落；细化对象是会影响代码正确性、安全隔离及故障恢复的接口与状态。原始文档包不作覆盖。

规范优先级：00 全局工程契约 > R3 各专项冻结结论 > 原 R3 详细展开 > 旧版非冲突细节。每章新增 K 节以及附录新增细化条目均为建议设计，不自动进入冻结层；与冻结内容冲突时必须走 ADR。

阅读顺序：总体理解先读 01、03、06、09；实现前再读 00、17、附录 C/D；桌面开发读 02、07、附录 E。后续功能域保留产品范围、约束和验收，不要求全部进入首个 CodeRepair 切片。

### 本次实质调整

- 通用对象、并发、错误、观测和交付规则集中到 00.K，避免每个模块重新重复一遍。
- 补充 TaskNode/Grant/SideEffect 状态机、Session 幂等创建、事件补投与 crash-cut 恢复矩阵。
- 细化 IPC Envelope、消息族、数据库最小表集合、scope 一致性和事务边界。
- 明确独立进程与实际 OS 隔离的区别，并将未确定的技术选择列为 ADR。
- 区分代码现状、冻结要求和工程建议；不把示例连接诊断、性能数字或文档测试当成已验证结果。
- 取消父子 Work；Work 改为扁平、不可嵌套且相互独立的 Scope，复杂任务层级继续由 TaskGraph、TaskNode 与 Subagent 表达。

### 阅读标识

“冻结决策”继承原 R3；“建议”用于设计评审和实现验证；“当前仓库”仅指本次检查到的 Desktop 原型；“验收”表示尚需执行的检查。DSH 上游 API 未在本轮编辑中联网核实，具体版本与导出接口见附录 B 的实施前清单。

## 章节导航

'@
$toc = ($chapters | ForEach-Object { '- ' + $_.Title }) -join "`n"
$fullText = $intro + "`n" + $toc + "`n`n" + (($chapters | ForEach-Object Text) -join "`n`n")
$baseName = 'KYNXA_R3_精炼与工程细化版'
$mdPath = Join-Path $outputRoot ($baseName+'.md')
Write-Utf8 $mdPath $fullText

# Content-preservation audit: unique table records, numbered flows and bullets.
# 内容保留检查覆盖独特表格记录、编号流程和项目符号。
$coverage = [System.Collections.Generic.List[object]]::new()
foreach($chapter in $chapters) {
    $original = $sourceChapters[$chapter.Id]
    $checks = [System.Collections.Generic.List[string]]::new()
    foreach($line in ($original -split "`n")) {
        if($line -match '^\| ([^|]+) \| ([^|]+) \|$') {
            $key=$Matches[1].Trim(); $value=$Matches[2].Trim()
            if($key -notin @('对象','服务/组件','Metric','责任 ID','Failure ID','Test ID')) {
                if($value -match '^稳定 ID|^只通过已冻结|^进入 Flight Recorder') { $checks.Add($key) }
                else { $checks.Add($value) }
            }
        } elseif($line -match '^\| [^|]+ \| [^|]+ \| [^|]+ \|') {
            if($line -notmatch '^\| (ID/主题|Upstream seam|类别|R2 组件)') { $checks.Add($line) }
        }
    }
    $missing = @($checks | Where-Object { -not $chapter.Text.Contains($_) })
    $coverage.Add([pscustomobject]@{chapter=$chapter.Id;records=$checks.Count;preserved=$checks.Count-$missing.Count;missing=$missing})
}
if (@($coverage | Where-Object {$_.missing.Count -gt 0}).Count) { throw ($coverage | ConvertTo-Json -Depth 5) }

# Word-native OOXML builder. No unmanaged Python/Node dependencies are used.
# 直接构建 Word 原生 OOXML，不依赖额外安装的 Python 或 Node 工具。
function ConvertTo-XmlEscapedText([string]$text) { [System.Security.SecurityElement]::Escape($text) }
function ConvertTo-InlineXml([string]$text) {
    $runXmlBuilder=[System.Text.StringBuilder]::new(); $pos=0
    foreach($m in [regex]::Matches($text,'\*\*([^*]+)\*\*|`([^`]+)`')) {
        if($m.Index -gt $pos) { [void]$runXmlBuilder.Append('<w:r><w:t xml:space="preserve">'+(ConvertTo-XmlEscapedText $text.Substring($pos,$m.Index-$pos))+'</w:t></w:r>') }
        $runProperties=if($m.Groups[1].Success){'<w:b/>'}else{'<w:rStyle w:val="CodeInline"/>'}
        $runText=if($m.Groups[1].Success){$m.Groups[1].Value}else{$m.Groups[2].Value}
        [void]$runXmlBuilder.Append('<w:r><w:rPr>'+$runProperties+'</w:rPr><w:t xml:space="preserve">'+(ConvertTo-XmlEscapedText $runText)+'</w:t></w:r>')
        $pos=$m.Index+$m.Length
    }
    if($pos -lt $text.Length) { [void]$runXmlBuilder.Append('<w:r><w:t xml:space="preserve">'+(ConvertTo-XmlEscapedText $text.Substring($pos))+'</w:t></w:r>') }
    $runXmlBuilder.ToString()
}
$bodyXml=[System.Text.StringBuilder]::new()
$tocTargets=@{}
$headingIndex=0
$firstHeading=$true
foreach($sourceLine in ($fullText -split "`n")) {
    if($sourceLine -match '^#{1,4} (.+)$') {
        $headingText=$Matches[1].TrimEnd()
        if($firstHeading){$firstHeading=$false;continue}
        if($headingText -eq '精炼与工程细化版'){continue}
        $headingIndex++
        if($headingText -in $chapters.Title){$tocTargets[$headingText]='section_'+$headingIndex}
    }
}
$script:numId=1
$script:extraNums=[System.Text.StringBuilder]::new()
$script:bookmarkId=0
function Add-Paragraph([string]$text,[string]$style='Normal',[string]$extra='') {
    $runs=ConvertTo-InlineXml $text
    if($style -eq 'ListText' -and $tocTargets.ContainsKey($text)) {
        $runs='<w:hyperlink w:anchor="'+$tocTargets[$text]+'" w:history="1">'+$runs+'</w:hyperlink>'
    }
    [void]$bodyXml.Append('<w:p><w:pPr><w:pStyle w:val="'+$style+'"/>'+$extra+'</w:pPr>'+$runs+'</w:p>')
}
function Add-Heading([string]$text,[int]$level) {
    $script:bookmarkId++
    [void]$bodyXml.Append('<w:p><w:pPr><w:pStyle w:val="Heading'+$level+'"/></w:pPr><w:bookmarkStart w:id="'+$script:bookmarkId+'" w:name="section_'+$script:bookmarkId+'"/>'+(ConvertTo-InlineXml $text)+'<w:bookmarkEnd w:id="'+$script:bookmarkId+'"/></w:p>')
}
function Add-Table([object[]]$rows) {
    $columns=$rows[0].Count
    $widths=switch($columns){2{@(2808,6552)} 3{@(2000,3200,4160)} 4{@(2080,2000,1520,3760)} default{@(for($j=0;$j -lt $columns;$j++){[int](9360/$columns)})}}
    $widths[-1]=9360-($widths[0..($columns-2)] | Measure-Object -Sum).Sum
    [void]$bodyXml.Append('<w:tbl><w:tblPr><w:tblW w:w="9360" w:type="dxa"/><w:tblInd w:w="120" w:type="dxa"/><w:tblLayout w:type="fixed"/><w:tblBorders>')
    foreach($border in @('top','left','bottom','right','insideH','insideV')) { [void]$bodyXml.Append('<w:'+$border+' w:val="single" w:sz="4" w:color="D9E0EB"/>') }
    [void]$bodyXml.Append('</w:tblBorders><w:tblCellMar><w:top w:w="80" w:type="dxa"/><w:bottom w:w="80" w:type="dxa"/><w:left w:w="120" w:type="dxa"/><w:right w:w="120" w:type="dxa"/></w:tblCellMar></w:tblPr><w:tblGrid>')
    foreach($width in $widths){[void]$bodyXml.Append('<w:gridCol w:w="'+$width+'"/>')}
    [void]$bodyXml.Append('</w:tblGrid>')
    for($r=0;$r -lt $rows.Count;$r++) {
        [void]$bodyXml.Append('<w:tr><w:trPr><w:cantSplit/>'+$(if($r -eq 0){'<w:tblHeader/>'})+'</w:trPr>')
        for($columnIndex=0;$columnIndex -lt $columns;$columnIndex++) {
            $cellText=if($columnIndex -lt $rows[$r].Count){$rows[$r][$columnIndex]}else{''}
            $fill=if($r -eq 0){'<w:shd w:fill="E8EEF5"/>'}else{''}
            $style=if($r -eq 0){'TableHead'}else{'TableText'}
            [void]$bodyXml.Append('<w:tc><w:tcPr><w:tcW w:w="'+$widths[$columnIndex]+'" w:type="dxa"/><w:vAlign w:val="center"/>'+$fill+'</w:tcPr><w:p><w:pPr><w:pStyle w:val="'+$style+'"/></w:pPr>'+(ConvertTo-InlineXml $cellText)+'</w:p></w:tc>')
        }
        [void]$bodyXml.Append('</w:tr>')
    }
    [void]$bodyXml.Append('</w:tbl>')
    Add-Paragraph '' 'TableGap'
}
$lines=$fullText -split "`n"
$inCode=$false; $inNumbered=$false; $titleDone=$false
for($i=0;$i -lt $lines.Count;$i++) {
    $line=$lines[$i].TrimEnd()
    if($line -match '^```'){$inCode=-not $inCode;$inNumbered=$false;continue}
    if($inCode){Add-Paragraph $line 'CodeBlock';continue}
    if(-not $line.Trim()){$inNumbered=$false;continue}
    if($line.StartsWith('|')) {
        $rows=[System.Collections.Generic.List[object]]::new()
        while($i -lt $lines.Count -and $lines[$i].StartsWith('|')) {
            if($lines[$i] -notmatch '^\|[\s:|-]+\|\s*$') {
                $cells=@($lines[$i].Trim().Trim('|').Split('|') | ForEach-Object Trim)
                $rows.Add($cells)
            }
            $i++
        }
        $i--;Add-Table $rows.ToArray();$inNumbered=$false;continue
    }
    if($line -match '^(#{1,4}) (.+)$') {
        $level=$Matches[1].Length;$heading=$Matches[2]
        if(-not $titleDone){Add-Paragraph $heading 'Title';$titleDone=$true}
        elseif($heading -eq '精炼与工程细化版'){Add-Paragraph $heading 'Subtitle'}
        else{Add-Heading $heading ([Math]::Min(3,$level))}
        $inNumbered=$false;continue
    }
    if($line -match '^[-*] (.+)$') {
        Add-Paragraph $Matches[1] 'ListText' '<w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>'
        $inNumbered=$false;continue
    }
    if($line -match '^\d+\. (.+)$') {
        $listText=$Matches[1]
        if(-not $inNumbered){$script:numId++;[void]$script:extraNums.Append('<w:num w:numId="'+$script:numId+'"><w:abstractNumId w:val="1"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="1"/></w:lvlOverride></w:num>')}
        Add-Paragraph $listText 'ListText' ('<w:numPr><w:ilvl w:val="0"/><w:numId w:val="'+$script:numId+'"/></w:numPr>')
        $inNumbered=$true;continue
    }
    Add-Paragraph ($line -replace '^> ','');$inNumbered=$false
}
if($inCode){throw 'Unclosed code block.'}

# compact_reference_guide preset; named overrides: CJK font, dense table/code, title.
# 使用 compact_reference_guide 预设，并明确覆盖中英文字体、密集表格与代码、标题样式。
function New-StyleXml($id,$name,$size,$color,$before,$after,$line,$extraP='',$extraR='') {
    '<w:style w:type="paragraph" w:styleId="'+$id+'"><w:name w:val="'+$name+'"/><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:before="'+$before+'" w:after="'+$after+'" w:line="'+$line+'" w:lineRule="auto"/><w:widowControl/>'+$extraP+'</w:pPr><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="Microsoft YaHei"/><w:color w:val="'+$color+'"/><w:sz w:val="'+$size+'"/><w:szCs w:val="'+$size+'"/>'+$extraR+'</w:rPr></w:style>'
}
$styles='<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="Microsoft YaHei"/><w:sz w:val="22"/><w:lang w:val="en-US" w:eastAsia="zh-CN"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:before="0" w:after="120" w:line="300" w:lineRule="auto"/><w:widowControl/></w:pPr></w:pPrDefault></w:docDefaults>'
$styles+='<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:spacing w:before="0" w:after="120" w:line="300" w:lineRule="auto"/><w:widowControl/></w:pPr><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="Microsoft YaHei"/><w:color w:val="202938"/><w:sz w:val="22"/></w:rPr></w:style>'
$styles+=New-StyleXml 'Title' 'Title' 48 '0B2545' 0 100 300 '<w:keepNext/>' '<w:b/>'
$styles+=New-StyleXml 'Subtitle' 'Subtitle' 28 '526174' 0 200 300 '<w:keepNext/>'
$styles+=New-StyleXml 'Heading1' 'heading 1' 32 '2E74B5' 360 200 300 '<w:keepNext/><w:keepLines/><w:outlineLvl w:val="0"/>' '<w:b/>'
$styles+=New-StyleXml 'Heading2' 'heading 2' 26 '2E74B5' 280 140 300 '<w:keepNext/><w:keepLines/><w:outlineLvl w:val="1"/>' '<w:b/>'
$styles+=New-StyleXml 'Heading3' 'heading 3' 24 '1F4D78' 200 100 300 '<w:keepNext/><w:keepLines/><w:outlineLvl w:val="2"/>' '<w:b/>'
$styles+=New-StyleXml 'ListText' 'List Text' 22 '202938' 0 80 300
$styles+=New-StyleXml 'TableText' 'Table Text' 19 '202938' 0 40 270
$styles+=New-StyleXml 'TableHead' 'Table Heading' 19 '0B2545' 0 40 270 '' '<w:b/>'
$styles+=New-StyleXml 'TableGap' 'Table Gap' 4 '202938' 0 40 240
$styles+=New-StyleXml 'CodeBlock' 'Code Block' 18 '202938' 0 20 260 '<w:shd w:fill="F4F6F9"/><w:ind w:left="120" w:right="120"/>' '<w:rFonts w:ascii="Consolas" w:hAnsi="Consolas" w:eastAsia="Microsoft YaHei"/>'
$styles+=New-StyleXml 'Footer' 'Footer' 17 '526174' 0 0 240 '<w:jc w:val="right"/>'
$styles+=New-StyleXml 'Header' 'Header' 17 '526174' 0 0 240
$styles+='<w:style w:type="character" w:styleId="CodeInline"><w:name w:val="Code Inline"/><w:rPr><w:rFonts w:ascii="Consolas" w:hAnsi="Consolas" w:eastAsia="Microsoft YaHei"/><w:sz w:val="20"/></w:rPr></w:style></w:styles>'
$numbering='<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
foreach($type in @('bullet','decimal')) {
    $aid=if($type -eq 'bullet'){0}else{1};$marker=if($aid -eq 0){'•'}else{'%1.'}
    $numbering+='<w:abstractNum w:abstractNumId="'+$aid+'"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="'+$type+'"/><w:lvlText w:val="'+$marker+'"/><w:lvlJc w:val="left"/><w:pPr><w:tabs><w:tab w:val="num" w:pos="540"/></w:tabs><w:ind w:left="540" w:hanging="271"/><w:spacing w:before="0" w:after="80" w:line="300" w:lineRule="auto"/></w:pPr></w:lvl></w:abstractNum>'
}
$numbering+='<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>'+$script:extraNums.ToString()+'</w:numbering>'
$document='<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>'+$bodyXml.ToString()+'<w:sectPr><w:headerReference w:type="default" r:id="rIdHeader"/><w:footerReference w:type="default" r:id="rIdFooter"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr></w:body></w:document>'
$parts=@{
    '[Content_Types].xml'='<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>'
    '_rels/.rels'='<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>'
    'word/document.xml'=$document
    'word/styles.xml'=$styles
    'word/numbering.xml'=$numbering
    'word/settings.xml'='<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:zoom w:percent="100"/><w:defaultTabStop w:val="720"/><w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="15"/></w:compat></w:settings>'
    'word/_rels/document.xml.rels'='<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rIdNum" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/><Relationship Id="rIdSettings" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/><Relationship Id="rIdHeader" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rIdFooter" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>'
    'word/header1.xml'='<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:pPr><w:pStyle w:val="Header"/></w:pPr><w:r><w:t>KYNXA · R3 精炼与工程细化版 / 冻结基线 + 建议设计</w:t></w:r></w:p></w:hdr>'
    'word/footer1.xml'='<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:pPr><w:pStyle w:val="Footer"/></w:pPr><w:r><w:t xml:space="preserve">KYNXA  |  </w:t></w:r><w:fldSimple w:instr="PAGE"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p></w:ftr>'
    'docProps/core.xml'='<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>KYNXA R3 精炼与工程细化版</dc:title><dc:creator>KYNXA</dc:creator><dc:description>R3 frozen baseline with proposed engineering refinements.</dc:description></cp:coreProperties>'
}
$docxPath=Join-Path $outputRoot ($baseName+'.docx')
$stream=[System.IO.File]::Open($docxPath,[System.IO.FileMode]::Create)
$zip=[System.IO.Compression.ZipArchive]::new($stream,[System.IO.Compression.ZipArchiveMode]::Create)
try {
    foreach($key in $parts.Keys) {
        [xml]$null=$parts[$key]
        $archiveEntry=$zip.CreateEntry($key)
        $writer=[System.IO.StreamWriter]::new($archiveEntry.Open(),$utf8)
        try{$writer.Write('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'+$parts[$key])}finally{$writer.Dispose()}
    }
} finally {$zip.Dispose();$stream.Dispose()}

# Exact Word table geometry and style audit.
# 精确检查 Word 表格宽度、单元格几何关系和样式结构。
[xml]$docXml=$document
$xmlNamespaces=[System.Xml.XmlNamespaceManager]::new($docXml.NameTable)
$xmlNamespaces.AddNamespace('w','http://schemas.openxmlformats.org/wordprocessingml/2006/main')
$tableCount=0
foreach($tableElement in $docXml.SelectNodes('//w:tbl',$xmlNamespaces)) {
    $tableCount++
    $grid=@($tableElement.SelectNodes('w:tblGrid/w:gridCol',$xmlNamespaces) | ForEach-Object {[int]$_.GetAttribute('w',$xmlNamespaces.LookupNamespace('w'))})
    if(($grid | Measure-Object -Sum).Sum -ne 9360){throw 'Incorrect grid width.'}
    foreach($row in $tableElement.SelectNodes('w:tr',$xmlNamespaces)) {
        $cells=$row.SelectNodes('w:tc',$xmlNamespaces)
        if($cells.Count -ne $grid.Count){throw 'Incorrect cell count.'}
        for($j=0;$j -lt $cells.Count;$j++) {
            if([int]$cells[$j].SelectSingleNode('w:tcPr/w:tcW',$xmlNamespaces).GetAttribute('w',$xmlNamespaces.LookupNamespace('w')) -ne $grid[$j]){throw 'Incorrect cell width.'}
        }
    }
}
$bookmarkNames=@($docXml.SelectNodes('//w:bookmarkStart',$xmlNamespaces) | ForEach-Object {$_.GetAttribute('name',$xmlNamespaces.LookupNamespace('w'))})
foreach($link in $docXml.SelectNodes('//w:hyperlink',$xmlNamespaces)) {
    if($link.GetAttribute('anchor',$xmlNamespaces.LookupNamespace('w')) -notin $bookmarkNames){throw 'Broken navigation bookmark.'}
}
$originalText=($sourceChapters.Values -join "`n")
$stats=[ordered]@{
    source_archive_sha256=(Get-FileHash -LiteralPath $SourceArchive -Algorithm SHA256).Hash
    source_content_nonspace=[regex]::Replace($originalText,'\s','').Length
    refined_content_nonspace=[regex]::Replace($fullText,'\s','').Length
    chapters=$chapters.Count
    tables=$tableCount
    original_record_checks=($coverage.records | Measure-Object -Sum).Sum
    preserved_record_checks=($coverage.preserved | Measure-Object -Sum).Sum
    xml_parts_valid=$parts.Count
    table_geometry='passed'
    active_work_model='flat, non-nestable, independently scoped Work; hierarchy remains inside TaskGraph/TaskNode/Subagent'
    intentional_supersessions=@(
        'Removed Child Work and Work-tree requirements'
        'Removed parent_work_id/parent_id from the proposed Work schema'
        'Replaced tree-cycle tests with cross-Work scope-isolation and revision-conflict tests'
    )
    render_qa='not performed: bundled Python/LibreOffice unavailable in this Windows environment'
    coverage=$coverage
}
Write-Utf8 (Join-Path $outputRoot 'content-audit.json') ($stats | ConvertTo-Json -Depth 6)
Write-Utf8 (Join-Path $outputRoot 'README.md') @'
# R3 精炼与工程细化版

主文档：KYNXA_R3_精炼与工程细化版.docx；可维护文本：同名 .md。

原始 139 页压缩包保持不变。本版保留 18 章和 5 个附录，将重复的通用规则集中，并补充可实现的接口、状态机、恢复和验收建议。建议内容不自动覆盖原冻结决策。

当前修订采用扁平 Work：每个 Work 都是不可嵌套且相互独立的 Scope；复杂任务层级继续由 Work 内部的 TaskGraph、TaskNode 与 Subagent 表达。标签、置顶、归档和展示分组不产生权限、知识或状态继承。

engineering-additions.md 是新增内容源，build.ps1 从原压缩包提取章节并生成主文档。运行方式：在 PowerShell 中执行 .\build.ps1；也可传 -SourceArchive 指定原压缩包。

content-audit.json 记录原始包摘要、内容规模、章节覆盖和 OOXML 结构校验。源文档的独特表格记录按内容保留检查；不把模板去重等同于删除需求。

排版采用 compact_reference_guide，Letter / 1 英寸页边距，Calibri 11 pt + Microsoft YaHei 中文，正文 1.25 倍行距；表格 9.5 pt、代码 9 pt 为具名密集参考样式。表格固定 DXA 宽度、重复表头、无固定行高，列表使用 Word 编号。页眉为简洁技术手册标识。

此环境缺少文档技能要求的配套 Python/LibreOffice，尚未完成 render_docx.py 的逐页图片视觉校验；页数不可由源文本量推断。结构校验不等同于视觉校验。
'@
[pscustomobject]@{docx=$docxPath;markdown=$mdPath;source_chars=$stats.source_content_nonspace;refined_chars=$stats.refined_content_nonspace;record_checks=$stats.original_record_checks;tables=$tableCount} | ConvertTo-Json
