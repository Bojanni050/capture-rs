@echo off
powershell -NoProfile -WindowStyle Hidden -Command "Start-Process -FilePath 'capture' -ArgumentList 'start' -WindowStyle Hidden"
exit
