# Спецификация: Рефакторинг Vulkan/wgpu инициализации (Stage 3A)

**Дата:** 2026-05-03
**Статус:** Утверждено
**Цель:** Единый Vulkan instance/device для decode + render, zero-copy interop.

---

## Проблема текущей реализации (Stage 2.5)

Сейчас инициализация идет «wgpu first»:
```
wgpu::Instance::new()
  -> create_surface
  -> request_adapter
  -> request_device
  -> as_hal::<Vulkan>() -> ash::Device (extracted)
```

Извлеченные ash handles доступны, но **ash::Instance недоступен**. Это блокирует:
- `enumerate_physical_device_queue_family_properties` -> нельзя найти video decode queue family
- `enumerate_device_extension_properties` -> нельзя проверить `VK_KHR_video_decode_vp9`
- `vkGetPhysicalDeviceVideoCapabilitiesKHR` -> нельзя query video decode capabilities

Без `ash::Instance` Stage 3 (реальный VP9 decode) технически невозможен.

---

## Решение: UnifiedVulkanInstance

Инвертируем инициализацию: wgpu-hal создает Vulkan instance, wgpu оборачивает его.

### Архитектура

```
wgpu_hal::vulkan::Instance::init_with_callback()
  -> callback добавляет VK_KHR_video_* extensions
  -> создает ash::Entry + ash::Instance
  -> возвращает wgpu_hal::vulkan::Instance
    |
    v
wgpu::Instance::from_hal::<Vulkan>()
    |
    v
wgpu::Adapter (request_adapter)
    |
    v
wgpu_hal::vulkan::Adapter::open_with_callback()
  -> callback добавляет video decode device extensions + video queue family
  -> возвращает hal::OpenDevice<Vulkan>
    |
    v
wgpu::Adapter::create_device_from_hal::<Vulkan>()
  -> возвращает (wgpu::Device, wgpu::Queue)
```

### Ключевые API (wgpu 0.29 / wgpu-hal 0.29)

**Instance:**
```rust
// wgpu-hal init_with_callback позволяет модифицировать extensions
let hal_instance = unsafe {
    wgpu_hal::vulkan::Instance::init_with_callback(
        &instance_desc,
        Some(Box::new(|args: CreateInstanceCallbackArgs| {
            args.extensions.push(ash::khr::video_queue::NAME);
            args.extensions.push(ash::khr::video_decode_queue::NAME);
            // VK_KHR_video_decode_vp9 — device extension, не instance
        })),
    )?
};

// wgpu::Instance из hal
let wgpu_instance = unsafe {
    wgpu::Instance::from_hal::<wgpu_hal::api::Vulkan>(hal_instance)
};
```

**Device:**
```rust
// Получаем hal adapter из wgpu adapter
let hal_adapter = unsafe { wgpu_adapter.as_hal::<Vulkan>() }.unwrap();

// Открываем device с callback — добавляем video decode extensions
let hal_open_device = unsafe {
    hal_adapter.open_with_callback(
        features,
        &limits,
        &memory_hints,
        Some(Box::new(|args: CreateDeviceCallbackArgs| {
            args.extensions.push(ash::khr::video_queue::NAME);
            args.extensions.push(ash::khr::video_decode_queue::NAME);
            args.extensions.push(ash::khr::video_decode_vp9::NAME);
            // Добавляем video decode queue family в queue_create_infos
            // Добавляем VkPhysicalDeviceVideoDecodeVP9FeaturesKHR в pNext
        })),
    )?
};

// Создаем wgpu device из hal device
let (wgpu_device, wgpu_queue) = unsafe {
    wgpu_adapter.create_device_from_hal::<Vulkan>(hal_open_device, &desc)?
};
```

---

## Компоненты

### 1. UnifiedVulkanInstance (crates/video-vulkan/src/instance.rs) — НОВЫЙ

Владеет `wgpu::Instance` и предоставляет доступ к ash-level операциям.

```rust
pub struct UnifiedVulkanInstance {
    pub wgpu_instance: wgpu::Instance,
}

impl UnifiedVulkanInstance {
    pub fn new() -> Result<Self>;
    
    /// Получить &ash::Instance через wgpu_hal guard
    pub fn with_ash_instance<F, R>(&self, f: F) -> Option<R>
    where F: FnOnce(&ash::Instance) -> R;
    
    /// Получить &ash::Entry через wgpu_hal guard
    pub fn with_entry<F, R>(&self, f: F) -> Option<R>
    where F: FnOnce(&ash::Entry) -> R;
}
```

### 2. Обновление VulkanVideoDevice (crates/video-vulkan/src/device.rs)

```rust
pub struct VulkanVideoDevice {
    pub ash_device: ash::Device,
    pub physical_device: vk::PhysicalDevice,
    pub graphics_queue: vk::Queue,
    pub graphics_queue_family: u32,
    pub video_decode_queue: Option<vk::Queue>,
    pub video_decode_queue_family: Option<u32>,
    pub has_vp9_decode: bool,
}
```

Инициализация:
1. Из `wgpu::Device` через `as_hal::<Vulkan>()` получаем `raw_device()`, `raw_physical_device()`, `queue_family_index()`.
2. Из `UnifiedVulkanInstance` получаем `&ash::Instance`.
3. Через `raw_instance` вызываем `get_physical_device_queue_family_properties(physical_device)`.
4. Ищем queue family с `QUEUE_VIDEO_DECODE_BIT_KHR`.
5. Если найден — создаем отдельный `vk::Queue` для video decode (или используем ту же очередь, если совпадает).
6. Проверяем `VK_KHR_video_decode_vp9` через `enumerate_device_extension_properties`.
7. Query video capabilities через `vkGetPhysicalDeviceVideoCapabilitiesKHR`.

### 3. Обновление GpuContext (crates/app-egui/src/render.rs)

```rust
pub struct GpuContext {
    pub unified: UnifiedVulkanInstance,
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub surface_format: wgpu::TextureFormat,
}
```

`GpuContext::new()`:
1. Создает `UnifiedVulkanInstance::new()`.
2. `wgpu_instance.create_surface(window)`.
3. `request_adapter` как раньше.
4. Device creation через `open_with_callback` + `create_device_from_hal`.

### 4. Обновление VulkanVideoDecoder (crates/video-vulkan/src/decoder.rs)

```rust
pub unsafe fn new(
    unified: &UnifiedVulkanInstance,
    wgpu_device: &wgpu::Device,
    wgpu_queue: &wgpu::Queue,
) -> Result<Self> { ... }
```

Получает `VulkanVideoDevice` из `UnifiedVulkanInstance`, который теперь может реально определить `has_vp9_decode_support()`.

---

## Интеграция

### Sequence diagram

```
main()
  |
  v
UnifiedVulkanInstance::new()
  |
  +-- wgpu_hal::vulkan::Instance::init_with_callback()
  |     +-- ash::Entry::load()
  |     +-- callback: push VK_KHR_video_queue, VK_KHR_video_decode_queue
  |     +-- vkCreateInstance()
  |     +-- return hal_instance
  |
  +-- wgpu::Instance::from_hal::<Vulkan>(hal_instance)
  |
  v
GpuContext::new(unified, window)
  |
  +-- unified.wgpu_instance.create_surface(window)
  |
  +-- request_adapter()
  |
  +-- request_device_with_video()
  |     +-- hal_adapter.open_with_callback()
  |     |     +-- callback: push video decode extensions
  |     |     +-- callback: add video decode queue family
  |     |     +-- callback: add VkPhysicalDeviceVideoDecodeVP9FeaturesKHR to pNext
  |     |     +-- vkCreateDevice()
  |     |     +-- return hal::OpenDevice
  |     |
  |     +-- wgpu_adapter.create_device_from_hal(hal_open_device)
  |     +-- return (wgpu::Device, wgpu::Queue)
  |
  v
VulkanVideoDecoder::new(&unified, &device, &queue)
  |
  +-- VulkanVideoDevice::from_unified(unified, wgpu_device)
  |     +-- as_hal::<Vulkan>() -> raw_device, raw_physical_device
  |     +-- with_ash_instance() -> raw_instance
  |     +-- enumerate_physical_device_queue_family_properties()
  |     +-- find VIDEO_DECODE_BIT_KHR queue family
  |     +-- enumerate_device_extension_properties() -> check VP9
  |     +-- get_physical_device_video_capabilitiesKHR() -> verify
  |
  +-- StubTextureCache (placeholder до Stage 3B)
```

---

## Риски

### Риск 1: wgpu internal code и video extensions
wgpu-hal Vulkan backend не знает про `VK_KHR_video_*`. Это instance extensions, wgpu-hal просто передает их в Vulkan — риск низкий. Но device extensions (`VK_KHR_video_decode_queue`) требуются для decode commands. Мы добавляем их через callback, wgpu-hal создаст `ash::Device` с ними. wgpu::Device будет работать нормально, т.к. video extensions не влияют на графику.

### Риск 2: ash::Instance lifetime
`wgpu_hal::Instance::init_with_callback` владеет `ash::Instance`. При drop `wgpu::Instance` будет вызван `destroy_instance`. Мы извлекаем `&ash::Instance` через guard только пока wgpu instance жива. Безопасно.

### Риск 3: Queue family index
`wgpu::Device` создается с одним queue family (graphics). Video decode может требовать другой queue family. `open_with_callback` позволяет добавить дополнительный queue family в `queue_create_infos`. wgpu-hal создаст `ash::Device` с обеими queue families. Но `wgpu::Queue` будет связана только с graphics queue. Для video decode commands нам понадобится отдельный `vk::Queue`, который мы получим через `vkGetDeviceQueue` из `ash::Device`.

### Риск 4: PhysicalDeviceFeatures pNext chain
`VkPhysicalDeviceVideoDecodeVP9FeaturesKHR` нужно добавить в pNext chain `vk::DeviceCreateInfo`. `CreateDeviceCallbackArgs.create_info` имеет тип `vk::DeviceCreateInfo<'pnext>`, и callback позволяет модифицировать pNext chain.

---

## Acceptance Criteria

- [ ] `UnifiedVulkanInstance` создает wgpu::Instance из wgpu_hal::vulkan::Instance::init_with_callback
- [ ] Video queue extensions добавляются в instance creation callback
- [ ] Video decode device extensions добавляются в device creation callback
- [ ] Video decode queue family обнаруживается и добавляется в device creation
- [ ] `VulkanVideoDevice::has_vp9_decode_support()` возвращает реальный результат (не false)
- [ ] `app-egui` компилируется и запускается (placeholder video + audio)
- [ ] Телеметрия показывает корректный backend status
