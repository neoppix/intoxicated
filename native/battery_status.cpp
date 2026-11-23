// The engine's battery report: `NativeGLInterface.reportBatteryStateChanged`
// and `NativeGLInterface.reportBatteryStatus`, and the Java classes the second
// one needs.
//
// Written from a behavioural spec and the engine's dex declarations as that
// spec records them. Rust decides what to report (`crates/intoxicated-runtime/src/
// power.rs`) and hands it over as one flat struct; this file only turns it
// into the object the engine reads.
//
// `com.roblox.engine.jni.model.BatteryStatus` is a plain object with a
// no-argument constructor and fifteen public fields, every one boxed so that
// null can mean "not known". A field the reading does not have stays null; it
// is never a zero, which the engine could only read as a battery at 0%, 0 mV.
// `batteryLow`, `batterySaverMode` and `energyCounter` have no reading at all
// and are always null. Which fields the engine reads has never been observed.
//
// The four box classes are registered here and nowhere else in this
// directory, so they stay registered whatever else changes: the engine may
// meet a boxed value outside the battery report too (INFERRED).

#include <jnivm.h>

#include <cstddef>
#include <cstdio>
#include <memory>
#include <string>

namespace intoxicated {

using jnivm::Class;
using jnivm::ENV;
using jnivm::Object;
using jnivm::String;

namespace {

template <class T>
auto to_jni(ENV* env, const std::shared_ptr<T>& p) {
    return jnivm::JNITypes<std::shared_ptr<T>>::ToJNIType(env, p);
}

/// `java.lang.Integer`, as far as unboxing goes.
class Integer : public Object {
public:
    jint value = 0;
    explicit Integer(jint v = 0) : value(v) {}
    jint intValue(ENV*) { return value; }
};

/// `java.lang.Boolean`.
class Boolean : public Object {
public:
    jboolean value = JNI_FALSE;
    explicit Boolean(jboolean v = JNI_FALSE) : value(v) {}
    jboolean booleanValue(ENV*) { return value; }
};

/// `java.lang.Long`.
class Long : public Object {
public:
    jlong value = 0;
    explicit Long(jlong v = 0) : value(v) {}
    jlong longValue(ENV*) { return value; }
};

/// `java.lang.Float`.
class Float : public Object {
public:
    jfloat value = 0.0f;
    explicit Float(jfloat v = 0.0f) : value(v) {}
    jfloat floatValue(ENV*) { return value; }
};

/// `com.roblox.engine.jni.model.BatteryStatus`.
class BatteryStatus : public Object {
public:
    std::shared_ptr<Boolean> present;
    std::shared_ptr<Integer> batteryPercentage;
    std::shared_ptr<Boolean> batterySaverMode;
    std::shared_ptr<Integer> chargeCounter;
    std::shared_ptr<Integer> currentAverage;
    std::shared_ptr<Integer> currentNow;
    std::shared_ptr<Long> energyCounter;
    std::shared_ptr<Integer> health;
    std::shared_ptr<Integer> plugged;
    std::shared_ptr<Integer> power;
    std::shared_ptr<Boolean> batteryLow;
    std::shared_ptr<Integer> status;
    std::shared_ptr<String> technology;
    /// Degrees Celsius.
    std::shared_ptr<Float> temperature;
    /// Millivolts.
    std::shared_ptr<Integer> voltage;

    /// `<init>()V`, in libjnivm's static-factory form; see
    /// `NativeFlagsInitResult` in jni_params.cpp for why.
    static std::shared_ptr<BatteryStatus> ctor(ENV* env, Class*) {
        auto p = std::make_shared<BatteryStatus>();
        to_jni(env, p);
        return p;
    }
};

} // namespace

void register_battery_classes(ENV* env) {
    // The boxes first: a field's descriptor is derived from its C++ type's
    // registered class, so `present` is only `Ljava/lang/Boolean;` once
    // Boolean is known.
    env->GetClass<Integer>("java/lang/Integer");
    env->GetClass("java/lang/Integer")->HookInstanceFunction(env, "intValue", &Integer::intValue);
    env->GetClass<Boolean>("java/lang/Boolean");
    env->GetClass("java/lang/Boolean")->HookInstanceFunction(env, "booleanValue", &Boolean::booleanValue);
    env->GetClass<Long>("java/lang/Long");
    env->GetClass("java/lang/Long")->HookInstanceFunction(env, "longValue", &Long::longValue);
    env->GetClass<Float>("java/lang/Float");
    env->GetClass("java/lang/Float")->HookInstanceFunction(env, "floatValue", &Float::floatValue);

    const char* name = "com/roblox/engine/jni/model/BatteryStatus";
    env->GetClass<BatteryStatus>(name);
    auto c = env->GetClass(name);
    c->Hook(env, "<init>", &BatteryStatus::ctor);
#define F(field) c->HookInstance(env, #field, &BatteryStatus::field)
    F(present);
    F(batteryPercentage);
    F(batterySaverMode);
    F(chargeCounter);
    F(currentAverage);
    F(currentNow);
    F(energyCounter);
    F(health);
    F(plugged);
    F(power);
    F(batteryLow);
    F(status);
    F(technology);
    F(temperature);
    F(voltage);
#undef F
}

} // namespace intoxicated

namespace intoxicated {
jnivm::ENV* process_env();
} // namespace intoxicated

/// The wire struct `intoxicated-linker-sys` sends
/// (`src/game_activity/battery.rs`, whose test pins the same offsets): a
/// has-flag before every value, 0 meaning absent.
struct IntoxicatedBatteryWire {
    int has_present, present;
    int has_percentage, percentage;
    int has_status, status;
    int has_health, health;
    int has_voltage_mv, voltage_mv;
    int has_current_now_ua, current_now_ua;
    int has_current_avg_ua, current_avg_ua;
    int has_charge_counter_uah, charge_counter_uah;
    int has_power_now_uw, power_now_uw;
    int has_technology;
    const char* technology;
    int has_temperature_c;
    float temperature_c;
    int has_plugged, plugged;
};
static_assert(sizeof(IntoxicatedBatteryWire) == 104, "wire size");
static_assert(alignof(IntoxicatedBatteryWire) == 8, "wire alignment");
static_assert(offsetof(IntoxicatedBatteryWire, has_technology) == 72, "wire layout");
static_assert(offsetof(IntoxicatedBatteryWire, technology) == 80, "wire layout");
static_assert(offsetof(IntoxicatedBatteryWire, has_temperature_c) == 88, "wire layout");
static_assert(offsetof(IntoxicatedBatteryWire, temperature_c) == 92, "wire layout");
static_assert(offsetof(IntoxicatedBatteryWire, has_plugged) == 96, "wire layout");
static_assert(offsetof(IntoxicatedBatteryWire, plugged) == 100, "wire layout");

namespace {

/// A local frame for the length of one call, popped however it ends: these
/// run again whenever the reading changes, and every object handed to the
/// engine lands in the current frame.
class LocalFrame {
public:
    explicit LocalFrame(JNIEnv* jni) : jni_(jni) { jni_->PushLocalFrame(16); }
    ~LocalFrame() { jni_->PopLocalFrame(nullptr); }
    LocalFrame(const LocalFrame&) = delete;
    LocalFrame& operator=(const LocalFrame&) = delete;

private:
    JNIEnv* jni_;
};

/// A Java exception the engine left pending, reported and cleared, so it does
/// not trip the next JNI call on this thread.
bool cleared_exception(JNIEnv* jni, const char* native, char* err, size_t err_len) {
    if (!jni->ExceptionCheck()) {
        return false;
    }
    jni->ExceptionClear();
    snprintf(err, err_len, "%s left a Java exception pending", native);
    return true;
}

template <class Box, class V>
std::shared_ptr<Box> box_if(int has, V value) {
    return has ? std::make_shared<Box>(value) : nullptr;
}

} // namespace

extern "C" {

/// `reportBatteryStateChanged(II)V`: Android's status and plugged values.
int intoxicated_report_battery_state_changed(void* fn, int status, int plugged, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jclass, jint, jint);
    auto* env = intoxicated::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or reportBatteryStateChanged is not exported");
        return -1;
    }
    try {
        JNIEnv* jni = env->GetJNIEnv();
        LocalFrame frame(jni);
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        reinterpret_cast<Call>(fn)(jni, (jclass)intoxicated::to_jni(env, cls), status, plugged);
        return cleared_exception(jni, "reportBatteryStateChanged", err, err_len) ? -1 : 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `reportBatteryStatus(Lcom/roblox/engine/jni/model/BatteryStatus;)V`.
int intoxicated_report_battery_status(void* fn, const IntoxicatedBatteryWire* w, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jclass, jobject);
    auto* env = intoxicated::process_env();
    if (!fn || !env || !w) {
        snprintf(err, err_len, "no JavaVM, no reading, or reportBatteryStatus is not exported");
        return -1;
    }
    try {
        using namespace intoxicated;
        JNIEnv* jni = env->GetJNIEnv();
        LocalFrame frame(jni);
        auto s = std::make_shared<BatteryStatus>();
        s->present = box_if<Boolean>(w->has_present, w->present ? JNI_TRUE : JNI_FALSE);
        s->batteryPercentage = box_if<Integer>(w->has_percentage, w->percentage);
        s->status = box_if<Integer>(w->has_status, w->status);
        s->health = box_if<Integer>(w->has_health, w->health);
        s->voltage = box_if<Integer>(w->has_voltage_mv, w->voltage_mv);
        s->currentNow = box_if<Integer>(w->has_current_now_ua, w->current_now_ua);
        s->currentAverage = box_if<Integer>(w->has_current_avg_ua, w->current_avg_ua);
        s->chargeCounter = box_if<Integer>(w->has_charge_counter_uah, w->charge_counter_uah);
        s->power = box_if<Integer>(w->has_power_now_uw, w->power_now_uw);
        s->temperature = box_if<Float>(w->has_temperature_c, w->temperature_c);
        s->plugged = box_if<Integer>(w->has_plugged, w->plugged);
        if (w->has_technology && w->technology) {
            s->technology = std::make_shared<String>(std::string(w->technology));
        }
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        reinterpret_cast<Call>(fn)(jni, (jclass)to_jni(env, cls), (jobject)to_jni(env, s));
        return cleared_exception(jni, "reportBatteryStatus", err, err_len) ? -1 : 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

} // extern "C"
