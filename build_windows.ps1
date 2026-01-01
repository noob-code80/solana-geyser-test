# Скрипт для сборки на Windows
# Требуется: Git for Windows (для sh.exe)

Write-Host "Проверка зависимостей..." -ForegroundColor Cyan

# Проверяем Git
$gitPaths = @(
    "C:\Program Files\Git\usr\bin",
    "C:\Program Files (x86)\Git\usr\bin"
)

$shPath = $null
foreach ($path in $gitPaths) {
    if (Test-Path "$path\sh.exe") {
        $shPath = $path
        Write-Host "✅ Найден sh.exe: $path" -ForegroundColor Green
        break
    }
}

if (-not $shPath) {
    Write-Host "❌ Git for Windows не найден!" -ForegroundColor Red
    Write-Host "Установите Git for Windows: https://git-scm.com/download/win" -ForegroundColor Yellow
    exit 1
}

# Добавляем в PATH
$env:PATH = "$shPath;$env:PATH"

# Устанавливаем PROTOC если есть
if (Test-Path "C:\protoc\bin\protoc.exe") {
    $env:PROTOC = "C:\protoc\bin\protoc.exe"
    Write-Host "✅ PROTOC установлен" -ForegroundColor Green
} else {
    Write-Host "⚠️  PROTOC не найден, будет использован vendored" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Запуск сборки..." -ForegroundColor Cyan
cargo build --release

